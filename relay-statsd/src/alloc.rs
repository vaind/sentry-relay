use crate::CounterMetric;

pub enum AllocCounters {
    /// Tracks memory allocated and deallocated
    Alloc,
}

impl CounterMetric for AllocCounters {
    fn name(&self) -> &'static str {
        match *self {
            AllocCounters::Alloc => "alloc",
        }
    }
}

memento::usecase! {
    pub enum RelayMemoryUseCase {
        default None,
        StoreNormalizer,
        MetricsAggregator,
        SessionMetricsExtraction,
        ProjectState,
    }

    impl memento::UseCase for RelayMemoryUseCase {
        fn on_alloc(&self, size: usize) {
            metric!(
                counter(AllocCounters::Alloc) += size as i64,
                use_case = self.as_str()
            );
        }

        fn on_dealloc(&self, size: usize) {
            metric!(
                counter(AllocCounters::Alloc) -= size as i64,
                use_case = self.as_str()
            );
        }
    }
}

impl RelayMemoryUseCase {
    fn as_str(&self) -> &'static str {
        match self {
            RelayMemoryUseCase::None => "none",
            RelayMemoryUseCase::StoreNormalizer => "store_normalizer",
            RelayMemoryUseCase::MetricsAggregator => "metrics_aggregator",
            RelayMemoryUseCase::SessionMetricsExtraction => "session_metrics_extraction",
        }
    }
}

pub type Allocator = memento::Alloc<RelayMemoryUseCase>;
pub use memento::new as new_allocator;
