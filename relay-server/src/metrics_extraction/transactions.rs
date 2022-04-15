use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(feature = "processing")]
use {
    crate::metrics_extraction::conditional_tagging::run_conditional_tagging,
    crate::metrics_extraction::{utils, TaggingRule},
    relay_common::UnixTimestamp,
    relay_general::protocol::{AsPair, Event, EventType, Timestamp},
    relay_general::store::{
        get_breakdown_measurements, get_measurement, normalize_dist, validate_timestamps,
        BreakdownsConfig,
    },
    relay_metrics::{DurationUnit, Metric, MetricUnit, MetricValue},
    std::fmt,
};

/// The metric on which the user satisfaction threshold is applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SatisfactionMetric {
    Duration,
    Lcp,
    #[serde(other)]
    Unknown,
}

/// Configuration for a single threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SatisfactionThreshold {
    metric: SatisfactionMetric,
    threshold: f64,
}

/// Configuration for applying the user satisfaction threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SatisfactionConfig {
    /// The project-wide threshold to apply.
    project_threshold: SatisfactionThreshold,
    /// Transaction-specific overrides of the project-wide threshold.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    transaction_thresholds: BTreeMap<String, SatisfactionThreshold>,
}

/// Configuration for extracting metrics from transaction payloads.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TransactionMetricsConfig {
    extract_metrics: BTreeSet<String>,
    extract_custom_tags: BTreeSet<String>,
    satisfaction_thresholds: Option<SatisfactionConfig>,
}

#[cfg(feature = "processing")]
const METRIC_NAMESPACE: &str = "transactions";

#[cfg(feature = "processing")]
fn extract_transaction_status(transaction: &Event) -> Option<String> {
    use relay_general::{
        protocol::{Context, ContextInner},
        types::Annotated,
    };

    let contexts = transaction.contexts.value()?;
    let trace_context = match contexts.get("trace").map(Annotated::value) {
        Some(Some(ContextInner(Context::Trace(trace_context)))) => trace_context,
        _ => return None,
    };
    let span_status = trace_context.status.value()?;
    Some(span_status.to_string())
}

#[cfg(feature = "processing")]
fn extract_dist(transaction: &Event) -> Option<String> {
    let mut dist = transaction.dist.0.clone();
    normalize_dist(&mut dist);
    dist
}

/// Satisfaction value used for Apdex and User Misery
/// https://docs.sentry.io/product/performance/metrics/#apdex
#[cfg(feature = "processing")]
enum UserSatisfaction {
    Satisfied,
    Tolerated,
    Frustrated,
}

#[cfg(feature = "processing")]
impl fmt::Display for UserSatisfaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UserSatisfaction::Satisfied => write!(f, "satisfied"),
            UserSatisfaction::Tolerated => write!(f, "tolerated"),
            UserSatisfaction::Frustrated => write!(f, "frustrated"),
        }
    }
}

#[cfg(feature = "processing")]
impl UserSatisfaction {
    /// The frustration threshold is always four times the threshold
    /// (see https://docs.sentry.io/product/performance/metrics/#apdex)
    const FRUSTRATION_FACTOR: f64 = 4.0;

    fn from_value(value: f64, threshold: f64) -> Self {
        if value <= threshold {
            Self::Satisfied
        } else if value <= Self::FRUSTRATION_FACTOR * threshold {
            Self::Tolerated
        } else {
            Self::Frustrated
        }
    }
}

/// Get duration from timestamp and `start_timestamp`.
#[cfg(feature = "processing")]
fn get_duration_millis(start: &Timestamp, end: &Timestamp) -> f64 {
    let start = start.timestamp_millis();
    let end = end.timestamp_millis();

    end.saturating_sub(start) as f64
}

/// Extract the the satisfaction value depending on the actual measurement/duration value
/// and the configured threshold.
#[cfg(feature = "processing")]
fn extract_user_satisfaction(
    config: &Option<SatisfactionConfig>,
    transaction: &Event,
    start_timestamp: &Timestamp,
    end_timestamp: &Timestamp,
) -> Option<UserSatisfaction> {
    if let Some(config) = config {
        let threshold = transaction
            .transaction
            .value()
            .and_then(|name| config.transaction_thresholds.get(name))
            .unwrap_or(&config.project_threshold);
        if let Some(value) = match threshold.metric {
            SatisfactionMetric::Duration => {
                Some(get_duration_millis(start_timestamp, end_timestamp))
            }
            SatisfactionMetric::Lcp => get_measurement(transaction, "lcp"),
            SatisfactionMetric::Unknown => None,
        } {
            return Some(UserSatisfaction::from_value(value, threshold.threshold));
        }
    }
    None
}

#[cfg(feature = "processing")]
pub fn extract_transaction_metrics(
    config: &TransactionMetricsConfig,
    breakdowns_config: Option<&BreakdownsConfig>,
    conditional_tagging_config: &[TaggingRule],
    event: &Event,
    target: &mut Vec<Metric>,
) -> bool {
    if config.extract_metrics.is_empty() {
        relay_log::trace!("dropping all transaction metrics because of empty allow-list");
        return false;
    }

    let before_len = target.len();

    let push_metric = |metric: Metric| {
        if config.extract_metrics.contains(&metric.name) {
            target.push(metric);
        } else {
            relay_log::trace!("dropping metric {} because of allow-list", metric.name);
        }
    };

    extract_transaction_metrics_inner(config, breakdowns_config, event, push_metric);

    let added_slice = &mut target[before_len..];
    run_conditional_tagging(event, conditional_tagging_config, added_slice);
    !added_slice.is_empty()
}

#[cfg(feature = "processing")]
fn extract_transaction_metrics_inner(
    config: &TransactionMetricsConfig,
    breakdowns_config: Option<&BreakdownsConfig>,
    event: &Event,
    mut push_metric: impl FnMut(Metric),
) {
    if event.ty.value() != Some(&EventType::Transaction) {
        return;
    }

    let (start_timestamp, end_timestamp) = match validate_timestamps(event) {
        Ok(pair) => pair,
        Err(_) => {
            return; // invalid transaction
        }
    };

    let unix_timestamp = match UnixTimestamp::from_datetime(end_timestamp.into_inner()) {
        Some(ts) => ts,
        None => return,
    };

    let mut tags = BTreeMap::<String, String>::new();
    if let Some(release) = event.release.as_str() {
        tags.insert("release".to_owned(), release.to_owned());
    }
    if let Some(dist) = extract_dist(event) {
        tags.insert("dist".to_owned(), dist);
    }
    if let Some(environment) = event.environment.as_str() {
        tags.insert("environment".to_owned(), environment.to_owned());
    }
    if let Some(transaction) = event.transaction.as_str() {
        tags.insert("transaction".to_owned(), transaction.to_owned());
    }

    if !config.extract_custom_tags.is_empty() {
        // XXX(slow): event tags are a flat array
        if let Some(event_tags) = event.tags.value() {
            for tag_entry in &**event_tags {
                if let Some(entry) = tag_entry.value() {
                    let (key, value) = entry.as_pair();
                    if let (Some(key), Some(value)) = (key.as_str(), value.as_str()) {
                        if config.extract_custom_tags.contains(key) {
                            tags.insert(key.to_owned(), value.to_owned());
                        }
                    }
                }
            }
        }
    }

    // Measurements
    if let Some(measurements) = event.measurements.value() {
        for (measurement_name, annotated) in measurements.iter() {
            let measurement = match annotated.value().and_then(|m| m.value.value()) {
                Some(measurement) => *measurement,
                None => continue,
            };

            let mut tags = tags.clone();
            if let Some(rating) = get_measurement_rating(measurement_name, measurement) {
                tags.insert("measurement_rating".to_owned(), rating);
            }

            push_metric(Metric::new_mri(
                METRIC_NAMESPACE,
                format_args!("measurements.{}", measurement_name),
                MetricUnit::None,
                MetricValue::Distribution(measurement),
                unix_timestamp,
                tags,
            ));
        }
    }

    // Breakdowns
    if let Some(breakdowns_config) = breakdowns_config {
        for (breakdown, measurements) in get_breakdown_measurements(event, breakdowns_config) {
            for (measurement_name, annotated) in measurements.iter() {
                let measurement = match annotated.value().and_then(|m| m.value.value()) {
                    Some(measurement) => *measurement,
                    None => continue,
                };

                push_metric(Metric::new_mri(
                    METRIC_NAMESPACE,
                    format_args!("breakdowns.{}.{}", breakdown, measurement_name),
                    MetricUnit::None,
                    MetricValue::Distribution(measurement),
                    unix_timestamp,
                    tags.clone(),
                ));
            }
        }
    }

    let user_satisfaction = extract_user_satisfaction(
        &config.satisfaction_thresholds,
        event,
        start_timestamp,
        end_timestamp,
    );
    let tags_with_satisfaction = match user_satisfaction {
        Some(satisfaction) => utils::with_tag(&tags, "satisfaction", satisfaction),
        None => tags.clone(),
    };

    // Duration
    let duration_millis = get_duration_millis(start_timestamp, end_timestamp);

    push_metric(Metric::new_mri(
        METRIC_NAMESPACE,
        "duration",
        MetricUnit::Duration(DurationUnit::MilliSecond),
        MetricValue::Distribution(duration_millis),
        unix_timestamp,
        match extract_transaction_status(event) {
            Some(status) => utils::with_tag(&tags, "transaction.status", status),
            None => tags_with_satisfaction.clone(),
        },
    ));

    // User
    if let Some(user) = event.user.value() {
        if let Some(user_id) = user.id.as_str() {
            push_metric(Metric::new_mri(
                METRIC_NAMESPACE,
                "user",
                MetricUnit::None,
                MetricValue::set_from_str(user_id),
                unix_timestamp,
                // A single user might end up in multiple satisfaction buckets when they have
                // some satisfying transactions and some frustrating transactions.
                // This is OK as long as we do not add these numbers *after* aggregation:
                //     <WRONG>total_users = uniqIf(user, satisfied) + uniqIf(user, tolerated) + uniqIf(user, frustrated)</WRONG>
                //     <RIGHT>total_users = uniq(user)</RIGHT>
                tags_with_satisfaction,
            ));
        }
    }
}

#[cfg(feature = "processing")]
fn get_measurement_rating(name: &str, value: f64) -> Option<String> {
    let rate_range = |meh_ceiling: f64, poor_ceiling: f64| {
        debug_assert!(meh_ceiling < poor_ceiling);
        if value < meh_ceiling {
            Some("good".to_owned())
        } else if value < poor_ceiling {
            Some("meh".to_owned())
        } else {
            Some("poor".to_owned())
        }
    };

    match name {
        "lcp" => rate_range(2500.0, 4000.0),
        "fcp" => rate_range(1000.0, 3000.0),
        "fid" => rate_range(100.0, 300.0),
        "cls" => rate_range(0.1, 0.25),
        _ => None,
    }
}

#[cfg(test)]
#[cfg(feature = "processing")]
mod tests {
    use super::*;

    use crate::metrics_extraction::TaggingRule;
    use relay_general::store::BreakdownsConfig;
    use relay_general::types::Annotated;
    use relay_metrics::DurationUnit;

    #[test]
    fn test_extract_transaction_metrics() {
        let json = r#"
        {
            "type": "transaction",
            "timestamp": "2021-04-26T08:00:00+0100",
            "start_timestamp": "2021-04-26T07:59:01+0100",
            "release": "1.2.3",
            "dist": "foo ",
            "environment": "fake_environment",
            "transaction": "mytransaction",
            "user": {
                "id": "user123"
            },
            "tags": {
                "fOO": "bar",
                "bogus": "absolutely"
            },
            "measurements": {
                "foo": {"value": 420.69},
                "lcp": {"value": 3000.0}
            },
            "spans": [
                {
                    "description": "<OrganizationContext>",
                    "op": "react.mount",
                    "parent_span_id": "8f5a2b8768cafb4e",
                    "span_id": "bd429c44b67a3eb4",
                    "start_timestamp": 1597976393.4619668,
                    "timestamp": 1597976393.4718769,
                    "trace_id": "ff62a8b040f340bda5d830223def1d81"
                }
            ]
        }
        "#;

        let breakdowns_config: BreakdownsConfig = serde_json::from_str(
            r#"
            {
                "span_ops": {
                    "type": "spanOperations",
                    "matches": ["react.mount"]
                }
            }
        "#,
        )
        .unwrap();

        let event = Annotated::from_json(json).unwrap();

        let mut metrics = vec![];
        extract_transaction_metrics(
            &TransactionMetricsConfig::default(),
            Some(&breakdowns_config),
            &[],
            event.value().unwrap(),
            &mut metrics,
        );
        assert_eq!(metrics, &[]);

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/measurements.foo@none",
                "d:transactions/measurements.lcp@none",
                "d:transactions/breakdowns.span_ops.ops.react.mount@none",
                "d:transactions/duration@millisecond",
                "s:transactions/user@none"
            ],
            "extractCustomTags": ["fOO"]
        }
        "#,
        )
        .unwrap();

        let mut metrics = vec![];
        extract_transaction_metrics(
            &config,
            Some(&breakdowns_config),
            &[],
            event.value().unwrap(),
            &mut metrics,
        );

        assert_eq!(metrics.len(), 5, "{:?}", metrics);

        assert_eq!(metrics[0].name, "d:transactions/measurements.foo@none");
        assert_eq!(metrics[1].name, "d:transactions/measurements.lcp@none");
        assert_eq!(
            metrics[2].name,
            "d:transactions/breakdowns.span_ops.ops.react.mount@none"
        );

        let duration_metric = &metrics[3];
        assert_eq!(duration_metric.name, "d:transactions/duration@millisecond");
        if let MetricValue::Distribution(value) = duration_metric.value {
            assert_eq!(value, 59000.0);
        } else {
            panic!(); // Duration must be set
        }

        let user_metric = &metrics[4];
        assert_eq!(user_metric.name, "s:transactions/user@none");
        assert!(matches!(user_metric.value, MetricValue::Set(_)));

        assert_eq!(metrics[1].tags["measurement_rating"], "meh");

        for metric in &metrics[0..4] {
            assert!(matches!(metric.value, MetricValue::Distribution(_)));
        }

        for metric in metrics {
            assert_eq!(metric.tags["release"], "1.2.3");
            assert_eq!(metric.tags["dist"], "foo");
            assert_eq!(metric.tags["environment"], "fake_environment");
            assert_eq!(metric.tags["transaction"], "mytransaction");
            assert_eq!(metric.tags["fOO"], "bar");
            assert!(!metric.tags.contains_key("bogus"));
        }
    }

    #[test]
    fn test_transaction_duration() {
        let json = r#"
        {
            "type": "transaction",
            "timestamp": "2021-04-26T08:00:00+0100",
            "start_timestamp": "2021-04-26T07:59:01+0100",
            "release": "1.2.3",
            "environment": "fake_environment",
            "transaction": "mytransaction",
            "contexts": {
                "trace": {
                    "status": "ok"
                }
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/duration@millisecond"
            ]
        }
        "#,
        )
        .unwrap();
        let mut metrics = vec![];
        extract_transaction_metrics(&config, None, &[], event.value().unwrap(), &mut metrics);

        assert_eq!(metrics.len(), 1);

        let duration_metric = &metrics[0];
        assert_eq!(duration_metric.name, "d:transactions/duration@millisecond");
        assert_eq!(
            duration_metric.unit,
            MetricUnit::Duration(DurationUnit::MilliSecond)
        );
        if let MetricValue::Distribution(value) = duration_metric.value {
            assert_eq!(value, 59000.0); // millis
        } else {
            panic!(); // Duration must be set
        }

        assert_eq!(duration_metric.tags.len(), 4);
        assert_eq!(duration_metric.tags["release"], "1.2.3");
        assert_eq!(duration_metric.tags["transaction.status"], "ok");
        assert_eq!(duration_metric.tags["environment"], "fake_environment");
        assert_eq!(duration_metric.tags["transaction"], "mytransaction");
    }

    #[test]
    fn test_user_satisfaction() {
        let json = r#"
        {
            "type": "transaction",
            "transaction": "foo",
            "start_timestamp": "2021-04-26T08:00:00+0100",
            "timestamp": "2021-04-26T08:00:01+0100",
            "user": {
                "id": "user123"
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/duration@millisecond",
                "s:transactions/user@none"
            ],
            "satisfactionThresholds": {
                "projectThreshold": {
                    "metric": "duration",
                    "threshold": 300
                },
                "extra_key": "should_be_ignored"
            }
        }
        "#,
        )
        .unwrap();
        let mut metrics = vec![];
        extract_transaction_metrics(&config, None, &[], event.value().unwrap(), &mut metrics);
        assert_eq!(metrics.len(), 2);

        for metric in metrics {
            assert_eq!(metric.tags.len(), 2);
            assert_eq!(metric.tags["satisfaction"], "tolerated");
        }
    }

    #[test]
    fn test_user_satisfaction_override() {
        let json = r#"
        {
            "type": "transaction",
            "transaction": "foo",
            "start_timestamp": "2021-04-26T08:00:00+0100",
            "timestamp": "2021-04-26T08:00:02+0100",
            "measurements": {
                "lcp": {"value": 41}
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/duration@millisecond"
            ],
            "satisfactionThresholds": {
                "projectThreshold": {
                    "metric": "duration",
                    "threshold": 300
                },
                "transactionThresholds": {
                    "foo": {
                        "metric": "lcp",
                        "threshold": 42
                    }
                }
            }
        }
        "#,
        )
        .unwrap();
        let mut metrics = vec![];
        extract_transaction_metrics(&config, None, &[], event.value().unwrap(), &mut metrics);
        assert_eq!(metrics.len(), 1);

        for metric in metrics {
            assert_eq!(metric.tags.len(), 2);
            assert_eq!(metric.tags["satisfaction"], "satisfied");
        }
    }

    #[test]
    fn test_user_satisfaction_catch_new_metric() {
        let json = r#"
        {
            "type": "transaction",
            "transaction": "foo",
            "start_timestamp": "2021-04-26T08:00:00+0100",
            "timestamp": "2021-04-26T08:00:02+0100",
            "measurements": {
                "lcp": {"value": 41}
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/duration@millisecond"
            ],
            "satisfactionThresholds": {
                "projectThreshold": {
                    "metric": "unknown_metric",
                    "threshold": 300
                }
            }
        }
        "#,
        )
        .unwrap();
        let mut metrics = vec![];
        extract_transaction_metrics(&config, None, &[], event.value().unwrap(), &mut metrics);
        assert_eq!(metrics.len(), 1);

        for metric in metrics {
            assert_eq!(metric.tags.len(), 1);
            assert!(!metric.tags.contains_key("satisfaction"));
        }
    }

    #[test]
    fn test_conditional_tagging() {
        let json = r#"
        {
            "type": "transaction",
            "transaction": "foo",
            "start_timestamp": "2021-04-26T08:00:00+0100",
            "timestamp": "2021-04-26T08:00:02+0100",
            "measurements": {
                "lcp": {"value": 41}
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/duration@millisecond"
            ]
        }
        "#,
        )
        .unwrap();

        let tagging_config: Vec<TaggingRule> = serde_json::from_str(
            r#"
        [
            {
                "condition": {"op": "gte", "name": "transaction.duration", "value": 9001},
                "targetMetrics": ["d:transactions/duration@millisecond"],
                "targetTag": "satisfaction",
                "tagValue": "frustrated"
            },
            {
                "condition": {"op": "gte", "name": "transaction.duration", "value": 666},
                "targetMetrics": ["d:transactions/duration@millisecond"],
                "targetTag": "satisfaction",
                "tagValue": "tolerated"
            },
            {
                "condition": {"op": "and", "inner": []},
                "targetMetrics": ["d:transactions/duration@millisecond"],
                "targetTag": "satisfaction",
                "tagValue": "satisfied"
            }
        ]
        "#,
        )
        .unwrap();

        let mut metrics = vec![];
        extract_transaction_metrics(
            &config,
            None,
            &tagging_config,
            event.value().unwrap(),
            &mut metrics,
        );
        assert_eq!(
            metrics,
            &[Metric::new_mri(
                METRIC_NAMESPACE,
                "duration",
                MetricUnit::Duration(DurationUnit::MilliSecond),
                MetricValue::Distribution(2000.0),
                UnixTimestamp::from_secs(1619420402),
                {
                    let mut tags = BTreeMap::new();
                    tags.insert("satisfaction".to_owned(), "tolerated".to_owned());
                    tags.insert("transaction".to_owned(), "foo".to_owned());
                    tags
                }
            )]
        );
    }

    #[test]
    fn test_conditional_tagging_lcp() {
        let json = r#"
        {
            "type": "transaction",
            "transaction": "foo",
            "start_timestamp": "2021-04-26T08:00:00+0100",
            "timestamp": "2021-04-26T08:00:02+0100",
            "measurements": {
                "lcp": {"value": 41}
            }
        }
        "#;

        let event = Annotated::from_json(json).unwrap();

        let config: TransactionMetricsConfig = serde_json::from_str(
            r#"
        {
            "extractMetrics": [
                "d:transactions/measurements.lcp@none"
            ]
        }
        "#,
        )
        .unwrap();

        let tagging_config: Vec<TaggingRule> = serde_json::from_str(
            r#"
        [
            {
                "condition": {"op": "gte", "name": "transaction.measurements.lcp", "value": 41},
                "targetMetrics": ["d:transactions/measurements.lcp@none"],
                "targetTag": "satisfaction",
                "tagValue": "frustrated"
            },
            {
                "condition": {"op": "gte", "name": "transaction.measurements.lcp", "value": 20},
                "targetMetrics": ["d:transactions/measurements.lcp@none"],
                "targetTag": "satisfaction",
                "tagValue": "tolerated"
            },
            {
                "condition": {"op": "and", "inner": []},
                "targetMetrics": ["d:transactions/measurements.lcp@none"],
                "targetTag": "satisfaction",
                "tagValue": "satisfied"
            }
        ]
        "#,
        )
        .unwrap();

        let mut metrics = vec![];
        extract_transaction_metrics(
            &config,
            None,
            &tagging_config,
            event.value().unwrap(),
            &mut metrics,
        );
        assert_eq!(
            metrics,
            &[Metric::new_mri(
                METRIC_NAMESPACE,
                "measurements.lcp",
                MetricUnit::None,
                MetricValue::Distribution(41.0),
                UnixTimestamp::from_secs(1619420402),
                {
                    let mut tags = BTreeMap::new();
                    tags.insert("satisfaction".to_owned(), "frustrated".to_owned());
                    tags.insert("measurement_rating".to_owned(), "good".to_owned());
                    tags.insert("transaction".to_owned(), "foo".to_owned());
                    tags
                }
            )]
        );
    }
}