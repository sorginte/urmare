/// Legacy graph-cache counters retained for API compatibility.
///
/// True incremental work is exposed through [`crate::IndexWorkStats`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphCacheStats {
    pub module_hits: usize,
    pub edge_hits: usize,
    pub edge_misses: usize,
}
