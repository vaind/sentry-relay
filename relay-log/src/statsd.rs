pub enum MemoryCounters {
    /// Tracks memory allocated and deallocated
    MemoryUsage,
}

impl CounterMetric for RelayCounters {
    fn name(&self) -> &'static str {
        match *self {
            LogCounters::MemoryUsage => "memory.usage",
        }
    }
}
