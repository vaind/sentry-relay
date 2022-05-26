use crate::CounterMetric;

pub enum AllocCounters {
    /// Tracks memory allocated and deallocated
    Alloc,
    DeallocGlitch,
    Error,
}

impl CounterMetric for AllocCounters {
    fn name(&self) -> &'static str {
        match *self {
            AllocCounters::Alloc => "alloc",
            AllocCounters::DeallocGlitch => "dealloc_glitch",
            AllocCounters::Error => "error",
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
            if matches!(self, RelayMemoryUseCase::None) {
                return;
            }

            let _usecase = Allocator::with_usecase(RelayMemoryUseCase::None);

            metric!(
                counter(AllocCounters::Alloc) += size as i64,
                use_case = self.as_str()
            );
        }

        fn on_dealloc(&self, size: usize) {
            if matches!(self, RelayMemoryUseCase::None) {
                return;
            }
            let _usecase = Allocator::with_usecase(RelayMemoryUseCase::None);

            metric!(
                counter(AllocCounters::Alloc) -= size as i64,
                use_case = self.as_str()
            );
        }

        fn on_dealloc_glitch() {
            let _usecase = Allocator::with_usecase(RelayMemoryUseCase::None);
            metric!(counter(AllocCounters::DeallocGlitch) += 1);
        }

        fn on_error() {
            // metric!(counter(AllocCounters::Error) += 1);
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
            RelayMemoryUseCase::ProjectState => "project_state",
        }
    }
}

pub type Allocator = memento::Alloc<RelayMemoryUseCase>;
pub use memento::new as new_allocator;
