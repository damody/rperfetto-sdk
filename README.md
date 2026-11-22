# rperfetto-sdk

純 Rust 的 Perfetto TrackEvent 產生套件，可當插件嵌入 app／遊戲。提供 Rust API 與
C API（`extern "C"`，cbindgen 產生 `include/rperfetto_sdk.h`，可編成
`staticlib`／`cdylib`）。

| 後端 | 旗標 | 做什麼 | 平台 |
|---|---|---|---|
| In-process | `IN_PROCESS` / `RPF_BACKEND_IN_PROCESS` | app 自己開 `TracingSession`，只錄自己的事件，輸出獨立 `.pftrace` | 全平台 |
| System | `SYSTEM` / `RPF_BACKEND_SYSTEM` | 以 producer 身分連 `traced`，註冊 `track_event` data source，事件進系統 trace，與 sched/CPU/SurfaceFlinger 同時間軸 | Linux、Android |
| ATRACE | `ATRACE` / `RPF_BACKEND_ATRACE` | 寫 `android.os.Trace` 格式到 ftrace `trace_marker` | Linux、Android |

三個後端可同時開（旗標 OR 起來）。

## 設計取捨：為什麼不 vendor 官方 SDK

選項比較：

| | vendor C++ amalgamation / C SDK (`perfetto/public/*`) + FFI | 純 Rust（本套件） |
|---|---|---|
| 相容性 | 官方實作，自動正確 | 需自己對齊 v53.1 protos 與 SMB ABI（有測試鎖住，見下） |
| 建置 | 要 C++17 toolchain、`perfetto.cc` 約 6 萬行；Android 要 NDK libc++，跨編譯／Windows MSVC 設定麻煩 | `cargo build`，只依賴 `prost`、`libc` |
| 體積 | 靜態庫數 MB，含用不到的 consumer／IPC host 等 | 小，只有 producer 端 |
| 嵌入遊戲 | C++ 例外／RTTI／全域建構子、與引擎自己的 libc++ 可能衝突 | 無 C++ runtime |
| 功能 | 全部（interning、startup tracing、SMB patching、TCP/vsock…） | 只做 TrackEvent 需要的子集 |

結論：TrackEvent producer 實際需要的協定很小且穩定（IPC framing + ProducerPort 8 個方法 +
SMB page/chunk 格式），自己實作比維護 C++ 建置鏈划算，所以選純 Rust。
protobuf 編碼用現成的 [`prost`](https://crates.io/crates/prost)：訊息結構在
`src/protos.rs` 手寫（不需要 protoc），`tests/proto_compat.rs` 會逐欄位比對
perfetto v53.1 的 `.proto` 原始碼（欄位號碼錯就測試失敗）。

### System backend 的 IPC 協定

`src/system/`，對照 perfetto `src/ipc/`、`src/tracing/ipc/producer/`、
`include/perfetto/ext/tracing/core/shared_memory_abi.h`：

1. UNIX socket：Android `/dev/socket/traced_producer`；Linux `/run/perfetto/traced-producer.sock`
   或 `/tmp/perfetto-producer`；可用 `$PERFETTO_PRODUCER_SOCK_NAME` 或 `InitArgs.producer_socket` 覆寫。
2. Frame：`u32 LE 長度 + IPCFrame`。`BindService("ProducerPort")` 取得 method id。
3. `InitializeConnection` → `RegisterDataSource{name:"track_event", will_notify_on_stop, handles_incremental_state_clear, track_event_descriptor(已註冊的 category)}`。
4. `GetAsyncCommand`（streaming）收命令：
   * `SetupTracing`：共享記憶體 fd 走 `SCM_RIGHTS`，`mmap` 起來（SMB）。
   * `StartDataSource`：讀 `DataSourceConfig.target_buffer` 與 `track_event_config` 的 enabled/disabled categories，開啟 category。
   * `Flush`：把各執行緒目前的 chunk 標成 complete，`CommitData{chunks_to_move, flush_request_id}`。
   * `StopDataSource`：關 category、flush、`NotifyDataSourceStopped`。
   * `ClearIncrementalState`：重送 track descriptor。
5. SMB 寫入：每頁切成單一 chunk（`kPageDiv1`），CAS page header 取得頁面
   （Free → BeingWritten → Complete），chunk header `{chunk_id, writer_id, packets(count:10, flags:6)}`，
   每個封包片段前綴 4 byte redundant varint；封包超過 chunk 時跨 chunk 分片
   （`kLastPacketContinuesOnNextChunk` / `kFirstPacketContinuesFromPrevChunk`）。
   完成的 chunk 由背景 commit thread 每 ~2ms 批次 `CommitData`。
6. 每條執行緒是一個 writer（一條 packet sequence）；writer id 在執行緒結束後回收，
   chunk_id 會延續（traced 用 `(producer, writer_id, chunk_id)` 當 key，重來會被當重複 chunk 丟棄）。
7. traced 沒在跑時（`retry_connect = true`，預設）背景每秒重試，traced 重啟也會重連。

不支援：SMB emulation（VM/TCP/vsock producer，`use_shmem_emulation`）、producer 提供的 SMB（startup tracing）、
out-of-band patching（我們封包長度寫入前就已知，不需要）。

### 平台可用性

| | In-process | System | ATRACE |
|---|---|---|---|
| Android | ✅ | ✅（一般 app 需 Android 11+ 才能連 traced；系統要 `persist.traced.enable=1`） | ✅（app 可寫 `trace_marker`） |
| Linux | ✅ | ✅（要有 traced 在跑） | ✅（需寫 `/sys/kernel/tracing/trace_marker` 權限，通常是 root） |
| Windows / macOS | ✅（時間用單調時鐘） | ❌（Perfetto 在 Windows 走 TCP + 具名共享記憶體，未實作；`init` 回 `Unsupported`） | ❌ |

Windows/macOS 只編譯 in-process 路徑（`#[cfg]` 隔離），本機沒有對應 target 尚未實際編譯驗證。

### System vs ATRACE

* System：完整 TrackEvent（參數、自訂巢狀 track、浮點 counter、flow），需要 trace config 含 `track_event` data source。
* ATRACE：只需 trace config 開 `ftrace/print`（或 atrace categories），但**沒有參數**（debug annotation 會丟掉）、
  counter 只能整數（浮點四捨五入）、自訂 track 的 slice 變成 `G|pid|track|name|cookie` async slice、
  執行緒名稱來自 kernel `comm`（`set_thread_name` 只影響 TrackEvent descriptor）。

## Rust API

```rust
use rperfetto_sdk::{self as pf, Arg, Backends, Category, CounterTrack, InitArgs, SessionConfig, Track, TracingSession};

static GAME: Category = Category::with_description("game", "Game loop");

pf::init(InitArgs { backends: Backends::IN_PROCESS | Backends::SYSTEM, ..Default::default() })?;
let session = TracingSession::start(SessionConfig::default())?;   // in-process 才需要

{
    let _frame = pf::trace_scope!(GAME, "frame", "index" => 42, "scene" => "forest"); // scoped guard
    pf::slice_begin(&GAME, "update", &[]);
    pf::slice_end(&GAME);
    pf::trace_instant!(GAME, "input", "key" => "W");
}
let loading = Track::named("Loading");                  // 自訂 track（process 底下）
pf::slice_begin_on(&GAME, loading, "load_chunk", &[Arg::new("chunk", 3)]);
pf::slice_end_on(&GAME, loading);
let fps = CounterTrack::with_unit("fps", "fps");
pf::counter(&GAME, fps, 59.9);
pf::trace_counter!(GAME, "entities", 120);              // 呼叫點自動建 counter track

GAME.set_enabled(false);                                // app 端 category 開關
pf::set_thread_name("render-thread");

session.stop_and_write("game.pftrace")?;                // 或 flush() / trace_bytes() / write_to_file()
pf::shutdown();
```

* Track：`Track::current_thread()`（預設）、`Track::process()`、`Track::named(name)`、
  `Track::named_with_parent(name, parent)`、`CounterTrack::new/with_unit/with_parent`。
* 參數型別：bool、i64、u64、f64、&str、pointer（`ArgValue`）。
* Category 是否錄製 = app 開關 ∧（in-process `SessionConfig.categories` 或 traced 的 `TrackEventConfig`）。
  規則與 Perfetto 相同：exact enabled → exact disabled → pattern enabled → pattern disabled → 預設開。
* `SliceGuard` 會把 end 送到 begin 當下的同一組後端。
* `SessionConfig { buffer_size_kb, ring_buffer, categories }`：ring buffer 滿了丟最舊、否則滿了停錄。
* 時間：Linux/Android 用 `CLOCK_BOOTTIME`（Perfetto 預設 trace clock），與 sched 直接對齊。

## C API

見 `include/rperfetto_sdk.h`、`examples/c/c_demo.c`。

```c
RpfInitArgs init = {0};
init.backends = RPF_BACKEND_IN_PROCESS;          /* | RPF_BACKEND_SYSTEM | RPF_BACKEND_ATRACE */
rpf_init(&init);
RpfCategory cat = rpf_category_register("game");
RpfSession* s = rpf_session_start(NULL);

RpfArg a = { .name = "index", .kind = RPF_ARG_TYPE_INT, .value.i = 1 };
rpf_slice_begin(cat, 0 /* 0 = 目前執行緒 */, "frame", &a, 1);
rpf_slice_end(cat, 0);
uint64_t gpu = rpf_track_named("GPU queue", 0);
uint64_t heap = rpf_track_counter("heap", 0, "bytes");
rpf_counter_i64(cat, heap, 4096);

rpf_session_stop(s);
rpf_session_write_file(s, "out.pftrace");
rpf_session_destroy(s);
rpf_shutdown();
```

連結：`librperfetto_sdk.a -lpthread -ldl -lm`（Android 不需 `-lpthread`）或 `librperfetto_sdk.so`。
修改 `src/ffi.rs` 後用 `scripts/gen_header.sh` 重新產生 header（需 `cbindgen`）。

## 建置

```bash
cargo build --release                      # rlib + staticlib + cdylib
```

```bash
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$HOME/.android-ndk/android-ndk-r29/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang cargo build --release --target aarch64-linux-android
```

```bash
examples/c/build.sh                        # host C 範例
```

```bash
TARGET=aarch64-linux-android examples/c/build.sh
```

## 範例

```bash
cargo run --example rust_demo -- out.pftrace          # in-process
cargo run --example rust_demo -- --system 10          # 寫進正在跑的 traced session
cargo run --example rust_demo -- --atrace 10          # ATRACE
examples/c/build/c_demo out.pftrace                   # C：同樣支援 --system / --atrace
cargo run --release --example fn_mips_demo -- --system 10   # 函數 MIPS（見下節）
cargo run --release --example bench                   # 每事件成本
```

錄 system trace 的 config 需要包含：

```
data_sources { config { name: "track_event"
  track_event_config { disabled_categories: "*" enabled_categories: "game" } } }
```

ATRACE 則需要 `ftrace_config { ftrace_events: "ftrace/print" }`。

## 測試與驗證

`cargo test`：

* 單元測試：category glob／filter 規則、SMB page/chunk 狀態轉換、跨 chunk 分片重組、頁面耗盡與回收。
* `tests/inprocess.rs`：prost 解碼輸出、descriptor 先於事件、category 過濾、ring／discard 模式；
  若有 `trace_processor_shell`（`$TRACE_PROCESSOR_SHELL`，預設 `perfetto-arm64/out/linux_release`），
  驗 slice 名稱／深度／category／track、參數、counter、process 名、stats 無 error/data_loss。
* `tests/proto_compat.rs`：100+ 個欄位號碼對照 perfetto v53.1 `.proto`（`$PERFETTO_SRC`）。
* `tests/func.rs`：`trace_fn!`／`func_begin`／`func_end` 的配對（category 中途切換、多餘的 end），
  以及 `sql/function_mips.sql` 在 trace_processor 上能執行。
* `tests/system_linux.rs`：啟動私有 `traced` + `perfetto` client，驗證 system 後端端到端
  （TrackEventConfig 過濾、6KB 封包跨 chunk、短命執行緒 writer id 回收、無 ABI violation）。

已在手機（k6995v1_64，Android 17）驗證：

* System：Rust 與 C 範例同時寫入系統 trace，`c_audio` 依 config 被過濾；
  slice 的 tid/pid 對到 kernel 的 thread／process，busy-wait slice 期間 sched 顯示該執行緒約 94% 時間在 CPU 上
  （`sched` 與 slice 同為 BOOTTIME 時間軸）。
* ATRACE：B/E、I、G/H（自訂 track）、C（counter）皆被 trace_processor 正確解析。

## 函數 MIPS（不需 root、不讀 perf counter）

App 端只標函數開頭／結尾（category `fn` 的 slice），**不讀任何 CPU 計數器**，所以不需要 root，也不需要
perf 權限。MIPS 由系統 trace 算：traced_perf 錄每顆 CPU 的 `instructions`／`cycles`，
`sql/function_mips.sql` 再依 sched 分攤到各函數 slice。做法跟 FPSGO 的「Game Thread MIPS」／`mtk_cpu_counter_interval` 相同。

標記方式：

* Rust：在函數開頭寫 `trace_fn!();`（slice 名稱自動取 `module::func`），也可寫 `trace_fn!(CAT, "name")`；
  或手動呼叫 `func::func_begin(&CAT, name)`／`func::func_end()`、`func::func_scope`。
* C：`rpf_func_begin(cat_or_NULL, name)`／`rpf_func_end()`。
* C/C++ 自動插樁：用 `-finstrument-functions` 編譯要量的程式碼，並連結
  [`examples/c/instrument_hooks.c`](examples/c/instrument_hooks.c)（實作 `__cyg_profile_func_enter/exit`，
  用 `dladdr` 取函數名；需 `-rdynamic`／`-Wl,--export-dynamic`），然後呼叫 `rpf_instrument_enable(NULL)`。
  範例：[`examples/c/fn_mips_demo.c`](examples/c/fn_mips_demo.c)、[`examples/fn_mips_demo.rs`](examples/fn_mips_demo.rs)。

錄製與分析：

```bash
adb shell setprop persist.traced.enable 1
```

```bash
adb shell setprop persist.traced_perf.enable 1
```

```bash
adb shell perfetto --txt -c - -o /data/misc/perfetto-traces/fn.pftrace < configs/function_mips.cfg
```

```bash
trace_processor_shell -q sql/function_mips.sql fn.pftrace
```

（`setprop` 那兩步只有 traced 預設沒開的裝置才需要，而且要 root；錄製本身用 shell 身分就可以。）

輸出 `function_mips`（每次呼叫一列）與 `function_mips_summary`（依函數名彙總）：`instructions`（含被呼叫者）、
`self_instructions`、`mips`（指令數／牆鐘時間）、`mips_cpu`（指令數／實際在 CPU 上的時間）、`ipc`、
`est_mcycles`（Σ 執行時間 × CPU 頻率，沒有 PMU 時也能用）。

`configs/function_mips.cfg` 的重點：

* `linux.perf` 用 `SW_CPU_CLOCK` 4 kHz 當 timebase，`HW_INSTRUCTIONS`／`HW_CPU_CYCLES` 當 follower（CPU-wide）。
  **不能設 `timestamp_clock: PERF_CLOCK_BOOTTIME`**：硬體 follower 用 BOOTTIME 會讓 perf_event_open 失敗，
  traced_perf 會直接丟掉這個 data source。用預設時鐘就好，trace_processor 會自動換算。
* cpu-clock 在 CPU idle 時不會觸發取樣，所以一個取樣區間可能橫跨 idle 時間。SQL 把每段的指令數只攤到該 CPU
  在區間內實際有排程的時間；直接用牆鐘時間平均攤，會把工作量抹到 idle 上。
* 計數是 per-CPU、user＋kernel 都算，所以執行緒執行期間 kernel 代它做的工作也會算進去。

手機實測（k6995v1_64，user build，**app 與錄製都以 shell uid 執行**）：

| 函數 | SQL `mips` | 原始 4 kHz 區間 MIPS | SQL `ipc` |
|---|---|---|---|
| `alu_work`（相依整數運算） | 7993 | 8483 | 1.69 |
| `fp_work`（sqrt/div 鏈） | 1145 | 953 | 0.27 |
| `pointer_chase`（64 MiB 隨機存取） | 242 | 197 | 0.09 |
| `frame`（以上全部加 sleep） | 1280 | — | 0.51 |

每個 frame 的總指令數 2060 萬，跟 `simpleperf stat` 量到的 2060 萬一致。解析度受取樣週期（250 µs）限制：
比幾個取樣週期還短的函數，量會跟相鄰函數混在一起（上表 pointer_chase 偏高，就是混到前面 alu_work 的量）。

## 效能

x86-64 Linux，in-process，`cargo run --release --example bench`：

| | 每次 |
|---|---|
| category 關閉 | ~0.6 ns |
| instant + 2 個參數 | ~160 ns |
| scoped slice（begin + end 兩個事件） | ~200 ns |

主要成本是 `clock_gettime(BOOTTIME)`（~17 ns）與 prost 編碼（~40 ns/事件）。事件寫進每條執行緒自己的
buffer（無跨執行緒競爭），steady state 下不配置記憶體（重用 scratch packet）。
目前沒有做 string interning，名稱每次都完整寫入。
