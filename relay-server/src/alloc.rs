use crate::statsd::RelayCounters;
use relay_statsd::metric;

memento::usecase! {
    pub enum MemoryUseCase {
        default None,
        StoreNormalizer,
        MetricsAggregator,
        SessionMetricsExtraction,
    }

    impl memento::UseCase for MemoryUseCase {
        fn on_alloc(&self, size: usize) {
            metric!(
                counter(RelayCounters::MemoryUsage) += size as i64,
                use_case = self.as_str()
            );
        }

        fn on_dealloc(&self, size: usize) {
            metric!(
                counter(RelayCounters::MemoryUsage) -= size as i64,
                use_case = self.as_str()
            );
        }
    }
}

impl MemoryUseCase {
    fn as_str(&self) -> &'static str {
        match self {
            MemoryUseCase::None => "none",
            MemoryUseCase::StoreNormalizer => "store_normalizer",
            MemoryUseCase::MetricsAggregator => "metrics_aggregator",
            MemoryUseCase::SessionMetricsExtraction => "session_metrics_extraction",
        }
    }
}

#[global_allocator]
pub static ALLOCATOR: memento::Alloc<MemoryUseCase> = memento::new!();
