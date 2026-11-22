-- Per-function MIPS from a system trace, for function slices emitted by
-- rperfetto-sdk (func_begin/func_end, trace_fn!, -finstrument-functions hooks).
--
-- Inputs (configs/function_mips.cfg):
--   * slices of category 'fn' on thread tracks (the app, no root needed);
--   * sched_switch (sched table);
--   * per-CPU cumulative counters 'instructions' / 'cycles' from linux.perf
--     (perf_counter_track), optionally power/cpu_frequency.
-- Method (as mtk_cpu_counter_interval / FPSGO "Game Thread MIPS"): each
-- CPU's counter delta between two samples is spread over the time that CPU
-- was busy (non-idle sched) within the interval -- the cpu-clock timebase
-- does not tick while the CPU idles, so an interval often spans idle time --
-- and the part overlapping a sched slice of the thread is given to the
-- thread. A function slice gets what its thread ran while it was open, split
-- by time within each running slice (resolution = sampling period).
-- Instructions are system-wide per CPU, so they include the kernel work
-- done on behalf of the thread while it was running.
--
--   trace_processor_shell -q sql/function_mips.sql trace.pftrace
--
-- Outputs: function_mips (one row per call) and function_mips_summary (per
-- function name). Change 'fn' below to use another category.

INCLUDE PERFETTO MODULE intervals.intersect;

CREATE PERFETTO TABLE _fm_fn AS
SELECT s.id, s.ts, s.dur, s.name, s.depth, s.parent_id, tt.utid
FROM slice s
JOIN thread_track tt ON s.track_id = tt.id
WHERE s.category = 'fn' AND s.dur > 0;

-- Sched slices of the threads that have function slices.
CREATE PERFETTO TABLE _fm_run AS
SELECT sc.id, sc.ts, sc.dur, sc.cpu, sc.utid
FROM sched sc
WHERE sc.dur > 0 AND sc.utid IN (SELECT DISTINCT utid FROM _fm_fn);

-- Per-CPU counter intervals: [ts, next ts) with the counter delta.
CREATE PERFETTO TABLE _fm_ctr AS
SELECT ROW_NUMBER() OVER (ORDER BY ts) AS id, ts, dur, cpu, name, delta
FROM (
  SELECT
    c.ts,
    LEAD(c.ts) OVER w - c.ts AS dur,
    t.cpu,
    t.name,
    LEAD(c.value) OVER w - c.value AS delta
  FROM counter c
  JOIN perf_counter_track t ON c.track_id = t.id
  WHERE t.name IN ('instructions', 'cycles')
  WINDOW w AS (PARTITION BY c.track_id ORDER BY c.ts)
)
WHERE dur > 0 AND delta >= 0;

CREATE PERFETTO TABLE _fm_ins AS
SELECT id, ts, dur, cpu, delta FROM _fm_ctr WHERE name = 'instructions';

CREATE PERFETTO TABLE _fm_cyc AS
SELECT id, ts, dur, cpu, delta FROM _fm_ctr WHERE name = 'cycles';

-- CPU frequency intervals (kHz), for an estimate when there is no PMU data.
CREATE PERFETTO TABLE _fm_freq AS
SELECT ROW_NUMBER() OVER (ORDER BY ts) AS id, ts, dur, cpu, khz
FROM (
  SELECT
    c.ts,
    COALESCE(LEAD(c.ts) OVER w, (SELECT end_ts FROM trace_bounds)) - c.ts AS dur,
    t.cpu,
    c.value AS khz
  FROM counter c
  JOIN cpu_counter_track t ON c.track_id = t.id
  WHERE t.name = 'cpufreq'
  WINDOW w AS (PARTITION BY c.track_id ORDER BY c.ts)
)
WHERE dur > 0;

-- Non-idle sched time of every CPU.
CREATE PERFETTO TABLE _fm_busy AS
SELECT id, ts, dur, cpu FROM sched WHERE dur > 0 AND utid != 0;

-- Busy time inside each counter interval (falls back to the interval length
-- when sched has no data for it).
CREATE PERFETTO TABLE _fm_ins_busy AS
SELECT i.id, COALESCE(b.busy, i.dur) AS busy
FROM _fm_ins i
LEFT JOIN (
  SELECT ii.id_1 AS id, SUM(ii.dur) AS busy
  FROM _interval_intersect!((_fm_busy, _fm_ins), (cpu)) ii
  GROUP BY ii.id_1
) b USING (id);

CREATE PERFETTO TABLE _fm_cyc_busy AS
SELECT c.id, COALESCE(b.busy, c.dur) AS busy
FROM _fm_cyc c
LEFT JOIN (
  SELECT ii.id_1 AS id, SUM(ii.dur) AS busy
  FROM _interval_intersect!((_fm_busy, _fm_cyc), (cpu)) ii
  GROUP BY ii.id_1
) b USING (id);

-- Pieces = (thread running slice ∩ counter interval), each with its own share
-- of the counter. Keeping them separate (rather than one average per running
-- slice) preserves the sampling resolution inside long running slices.
CREATE PERFETTO TABLE _fm_ins_piece AS
SELECT
  ROW_NUMBER() OVER (ORDER BY ii.ts) AS id,
  ii.ts,
  ii.dur,
  r.utid,
  i.delta * ii.dur * 1.0 / ib.busy AS v
FROM _interval_intersect!((_fm_run, _fm_ins), (cpu)) ii
JOIN _fm_run r ON r.id = ii.id_0
JOIN _fm_ins i ON i.id = ii.id_1
JOIN _fm_ins_busy ib ON ib.id = ii.id_1;

CREATE PERFETTO TABLE _fm_cyc_piece AS
SELECT
  ROW_NUMBER() OVER (ORDER BY ii.ts) AS id,
  ii.ts,
  ii.dur,
  r.utid,
  c.delta * ii.dur * 1.0 / cb.busy AS v
FROM _interval_intersect!((_fm_run, _fm_cyc), (cpu)) ii
JOIN _fm_run r ON r.id = ii.id_0
JOIN _fm_cyc c ON c.id = ii.id_1
JOIN _fm_cyc_busy cb ON cb.id = ii.id_1;

CREATE PERFETTO TABLE _fm_freq_piece AS
SELECT
  ROW_NUMBER() OVER (ORDER BY ii.ts) AS id,
  ii.ts,
  ii.dur,
  r.utid,
  f.khz * 1000.0 * ii.dur / 1e9 AS v
FROM _interval_intersect!((_fm_run, _fm_freq), (cpu)) ii
JOIN _fm_run r ON r.id = ii.id_0
JOIN _fm_freq f ON f.id = ii.id_1;

-- Running time of each function slice.
CREATE PERFETTO TABLE _fm_fn_run AS
SELECT ii.id_0 AS id, SUM(ii.dur) AS running_ns
FROM _interval_intersect!((_fm_fn, _fm_run), (utid)) ii
GROUP BY ii.id_0;

CREATE PERFETTO TABLE _fm_fn_ins AS
SELECT ii.id_0 AS id, SUM(p.v * ii.dur / p.dur) AS v
FROM _interval_intersect!((_fm_fn, _fm_ins_piece), (utid)) ii
JOIN _fm_ins_piece p ON p.id = ii.id_1
GROUP BY ii.id_0;

CREATE PERFETTO TABLE _fm_fn_cyc AS
SELECT ii.id_0 AS id, SUM(p.v * ii.dur / p.dur) AS v
FROM _interval_intersect!((_fm_fn, _fm_cyc_piece), (utid)) ii
JOIN _fm_cyc_piece p ON p.id = ii.id_1
GROUP BY ii.id_0;

CREATE PERFETTO TABLE _fm_fn_freq AS
SELECT ii.id_0 AS id, SUM(p.v * ii.dur / p.dur) AS v
FROM _interval_intersect!((_fm_fn, _fm_freq_piece), (utid)) ii
JOIN _fm_freq_piece p ON p.id = ii.id_1
GROUP BY ii.id_0;

CREATE PERFETTO TABLE _fm_fn_agg AS
SELECT
  f.id,
  r.running_ns,
  i.v AS instructions,
  c.v AS cycles,
  q.v AS est_cycles
FROM _fm_fn f
LEFT JOIN _fm_fn_run r USING (id)
LEFT JOIN _fm_fn_ins i USING (id)
LEFT JOIN _fm_fn_cyc c USING (id)
LEFT JOIN _fm_fn_freq q USING (id);

-- One row per function call.
--   instructions      inclusive of callees
--   self_instructions minus the direct 'fn' children
--   mips              instructions / wall duration (µs)
--   mips_cpu          instructions / time the thread was running (µs)
--   ipc               instructions / cycles
--   est_mcycles       Σ running time × CPU frequency (no PMU needed)
CREATE PERFETTO TABLE function_mips AS
SELECT
  f.id,
  f.ts,
  f.dur,
  f.name,
  f.depth,
  f.utid,
  COALESCE(a.running_ns, 0) AS running_ns,
  CAST(a.instructions AS INT) AS instructions,
  CAST(a.instructions - COALESCE((
    SELECT SUM(c.instructions) FROM _fm_fn_agg c JOIN _fm_fn cf ON cf.id = c.id WHERE cf.parent_id = f.id
  ), 0) AS INT) AS self_instructions,
  a.instructions * 1000.0 / f.dur AS mips,
  CASE WHEN a.running_ns > 0 THEN a.instructions * 1000.0 / a.running_ns END AS mips_cpu,
  CASE WHEN a.cycles > 0 THEN a.instructions / a.cycles END AS ipc,
  a.est_cycles / 1e6 AS est_mcycles
FROM _fm_fn f
LEFT JOIN _fm_fn_agg a ON a.id = f.id;

-- Per function name.
CREATE PERFETTO TABLE function_mips_summary AS
SELECT
  name,
  COUNT(*) AS calls,
  SUM(dur) AS total_dur,
  SUM(running_ns) AS total_running_ns,
  SUM(instructions) AS instructions,
  SUM(self_instructions) AS self_instructions,
  SUM(instructions) * 1000.0 / SUM(dur) AS mips,
  SUM(instructions) * 1000.0 / NULLIF(SUM(running_ns), 0) AS mips_cpu,
  AVG(ipc) AS avg_ipc,
  SUM(est_mcycles) AS est_mcycles
FROM function_mips
GROUP BY name
ORDER BY instructions DESC;

SELECT
  name, calls, total_dur, total_running_ns, instructions, self_instructions,
  ROUND(mips, 1) AS mips, ROUND(mips_cpu, 1) AS mips_cpu, ROUND(avg_ipc, 2) AS ipc,
  ROUND(est_mcycles, 1) AS est_mcycles
FROM function_mips_summary;
