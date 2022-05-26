use relay_statsd::{new_allocator, Allocator};

#[global_allocator]
static ALLOCATOR: Allocator = new_allocator!();
