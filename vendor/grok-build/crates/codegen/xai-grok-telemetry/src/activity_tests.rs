//! Uses a local static so the production gauges stay untouched.

use super::ActivityGauge;

#[test]
fn gauges_saturate_at_zero_and_guards_decrement_exactly_once_on_drop() {
    static GAUGE: ActivityGauge = ActivityGauge::new();
    GAUGE.inc();
    GAUGE.inc();
    assert_eq!(GAUGE.get(), 2);
    GAUGE.dec();
    GAUGE.dec();
    GAUGE.dec();
    assert_eq!(GAUGE.get(), 0, "a decrement below zero must saturate");

    let outer = GAUGE.enter();
    {
        let _inner = GAUGE.enter();
        assert_eq!(GAUGE.get(), 2);
    }
    assert_eq!(GAUGE.get(), 1, "the inner guard must release its slot");
    drop(outer);
    assert_eq!(GAUGE.get(), 0);
}

#[test]
fn concurrent_decrements_do_not_lose_updates() {
    let gauge = ActivityGauge::new();
    for _ in 0..4000 {
        gauge.inc();
    }
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..400 {
                    gauge.dec();
                }
            });
        }
    });
    assert_eq!(gauge.get(), 800);
}
