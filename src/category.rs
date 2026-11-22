//! Track-event categories.
//!
//! A category is enabled for a given backend ("sink") when
//! 1. the application has not switched it off ([`Category::set_enabled`]), and
//! 2. the sink is active and its category filter accepts it (the in-process
//!    [`SessionConfig`](crate::SessionConfig) or the `TrackEventConfig` sent by
//!    traced). ATRACE accepts every category.
//!
//! The per-sink result is cached in an atomic bitmask so the hot-path check is
//! a single relaxed load.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

pub(crate) const SINK_INPROC: u8 = 1;
pub(crate) const SINK_SYSTEM: u8 = 2;
pub(crate) const SINK_ATRACE: u8 = 4;
pub(crate) const SINK_PROTO: u8 = SINK_INPROC | SINK_SYSTEM;

pub struct Category {
    name: &'static str,
    description: &'static str,
    user_enabled: AtomicBool,
    registered: AtomicBool,
    state: AtomicU8,
}

/// Category filter with Perfetto's `TrackEventConfig` semantics (tags are not
/// supported). Rules, first match wins:
/// exact enabled → exact disabled → pattern enabled → pattern disabled →
/// enabled. Patterns support `*` wildcards (e.g. `"*"`, `"gfx.*"`).
#[derive(Clone, Debug, Default)]
pub struct CategoryFilter {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl CategoryFilter {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn accepts(&self, cat: &str) -> bool {
        let exact = |v: &Vec<String>| v.iter().any(|p| !p.contains('*') && p == cat);
        let pattern = |v: &Vec<String>| v.iter().any(|p| p.contains('*') && glob(p, cat));
        if exact(&self.enabled) {
            return true;
        }
        if exact(&self.disabled) {
            return false;
        }
        if pattern(&self.enabled) {
            return true;
        }
        if pattern(&self.disabled) {
            return false;
        }
        true
    }
}

fn glob(pat: &str, s: &str) -> bool {
    let (p, s) = (pat.as_bytes(), s.as_bytes());
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (usize::MAX, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] != b'*' && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            pi += 1;
            mark = si;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

struct Registry {
    cats: Vec<&'static Category>,
    dynamic: HashMap<String, &'static Category>,
    /// Active filter per sink bit (index = bit position).
    filters: [Option<CategoryFilter>; 3],
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            cats: Vec::new(),
            dynamic: HashMap::new(),
            filters: [None, None, None],
        })
    })
}

fn compute(reg: &Registry, c: &Category) -> u8 {
    if !c.user_enabled.load(Ordering::Relaxed) {
        return 0;
    }
    let mut m = 0;
    for (i, f) in reg.filters.iter().enumerate() {
        if let Some(f) = f {
            if f.accepts(c.name) {
                m |= 1 << i;
            }
        }
    }
    m
}

impl Category {
    /// Declares a category, typically as a `static`:
    /// `static RENDER: Category = Category::new("render");`
    pub const fn new(name: &'static str) -> Self {
        Self::with_description(name, "")
    }

    pub const fn with_description(name: &'static str, description: &'static str) -> Self {
        Category {
            name,
            description,
            user_enabled: AtomicBool::new(true),
            registered: AtomicBool::new(false),
            state: AtomicU8::new(0),
        }
    }

    /// Returns the (leaked, deduplicated) category with a runtime name. Used by
    /// the C API and for categories not known at compile time.
    pub fn dynamic(name: &str) -> &'static Category {
        let mut reg = registry().lock().unwrap();
        if let Some(c) = reg.dynamic.get(name) {
            return c;
        }
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        let c: &'static Category = Box::leak(Box::new(Category::new(leaked)));
        reg.dynamic.insert(name.to_string(), c);
        c.registered.store(true, Ordering::Relaxed);
        reg.cats.push(c);
        c.state.store(compute(&reg, c), Ordering::Release);
        c
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    /// Registers the category (so that it is advertised to traced and follows
    /// filter changes). Called implicitly on first use.
    pub fn register(&'static self) {
        if self.registered.load(Ordering::Acquire) {
            return;
        }
        let mut reg = registry().lock().unwrap();
        if self.registered.load(Ordering::Relaxed) {
            return;
        }
        reg.cats.push(self);
        self.state.store(compute(&reg, self), Ordering::Release);
        self.registered.store(true, Ordering::Release);
    }

    /// Bitmask of sinks this category is currently enabled for.
    #[inline]
    pub(crate) fn sinks(&'static self) -> u8 {
        if !self.registered.load(Ordering::Acquire) {
            self.register();
        }
        self.state.load(Ordering::Relaxed)
    }

    /// True if at least one active backend records this category.
    #[inline]
    pub fn is_enabled(&'static self) -> bool {
        self.sinks() != 0
    }

    /// Application-level on/off switch (default on). Off overrides every
    /// backend's configuration.
    pub fn set_enabled(&'static self, on: bool) {
        self.register();
        let reg = registry().lock().unwrap();
        self.user_enabled.store(on, Ordering::Relaxed);
        self.state.store(compute(&reg, self), Ordering::Release);
    }

    pub fn is_user_enabled(&self) -> bool {
        self.user_enabled.load(Ordering::Relaxed)
    }
}

/// Activates (`Some`) or deactivates (`None`) a sink and recomputes all
/// category states.
pub(crate) fn set_sink_filter(sink: u8, filter: Option<CategoryFilter>) {
    let mut reg = registry().lock().unwrap();
    reg.filters[sink.trailing_zeros() as usize] = filter;
    for c in reg.cats.iter() {
        c.state.store(compute(&reg, c), Ordering::Release);
    }
}

/// Names and descriptions of all registered categories.
pub fn registered_categories() -> Vec<(&'static str, &'static str)> {
    let reg = registry().lock().unwrap();
    reg.cats.iter().map(|c| (c.name, c.description)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches() {
        assert!(glob("*", "anything"));
        assert!(glob("gfx.*", "gfx.present"));
        assert!(!glob("gfx.*", "audio"));
        assert!(glob("a*c", "abbbc"));
        assert!(!glob("a*c", "abbb"));
    }

    #[test]
    fn filter_rules() {
        let f = CategoryFilter {
            enabled: vec!["render".into(), "net.*".into()],
            disabled: vec!["*".into(), "net.slow".into()],
        };
        assert!(f.accepts("render"));
        assert!(f.accepts("net.http"));
        assert!(!f.accepts("net.slow"));
        assert!(!f.accepts("audio"));
        assert!(CategoryFilter::all().accepts("x"));
    }
}
