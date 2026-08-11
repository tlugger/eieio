//! Scripting the virtual capabilities (SDK §6.1).
//!
//! The half of a block that a batch cannot exercise: a timer that fires, a pin that
//! changes, a store that refuses, a request that comes back. Each is scripted from the
//! host's side, because that is which side it happens on.

use eio_manifest::PropertyType;
use eio_sdk::PinLevel;
use eio_sdk::prelude::*;
use eio_test_host::{TestHost, Throttle};

/// A block exercising every capability, so one fixture covers all five.
#[block(
    name = "instrumented",
    inputs(in),
    outputs(out),
    capabilities(state, timer, gpio, i2c, http)
)]
struct Instrumented {
    #[prop(ty = "int", default = "100")]
    period_ms: Prop<i64>,
    ticks: i64,
    last_level: Option<i32>,
    last_status: Option<i32>,
    armed: Option<u32>,
}

impl Block for Instrumented {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        let period = self.period_ms.get_static(ctx)?;
        self.armed = Some(ctx.timers().repeating(period)?.get());
        ctx.gpio().mode(7, eio_sdk::Mode::InputPullup)?;
        ctx.gpio().watch(7, eio_sdk::Edge::Both)?;
        Ok(())
    }

    fn on_timer(&mut self, ctx: &mut Ctx, _timer: u32) -> BlockResult {
        self.ticks += 1;
        // Persistence is best-effort (ABI §7.2); a refusal is the block's to handle.
        ctx.state().put("ticks", &Value::Int(self.ticks))?;
        Ok(())
    }

    fn on_gpio(&mut self, _ctx: &mut Ctx, _watch: u32, value: i32) -> BlockResult {
        self.last_level = Some(value);
        Ok(())
    }

    fn on_http(&mut self, _ctx: &mut Ctx, _req: u32, status: i32, _body: &[u8]) -> BlockResult {
        self.last_status = Some(status);
        Ok(())
    }
}

fn host() -> TestHost<Instrumented> {
    let mut builder = TestHost::<Instrumented>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .property("period_ms", PropertyType::Int, "100");
    // The ids `start` will be handed: the timer, then the watch. Scripted before the
    // lifecycle runs, because `start` asks for them.
    builder = builder.scripted(|scripted| {
        scripted.id(1).id(2);
    });
    builder.start().expect("it starts")
}

#[test]
fn start_arms_a_timer_from_a_property_the_real_interpreter_evaluated() {
    let host = host();
    assert_eq!(host.block().armed, Some(1));
}

#[test]
fn a_fired_timer_reaches_the_block() {
    // The host drives the callback, because that is what a host does — a timer firing is
    // not something the block asks for and receives, it is something that happens to it.
    let mut host = host();
    host.fire_timer(1).expect("fired");
    host.fire_timer(1).expect("fired");
    assert_eq!(host.block().ticks, 2);
}

#[test]
fn err_throttled_can_be_injected_without_a_flash_budget() {
    // ABI §7.2's leaf-host refusal, which is otherwise reachable only on real hardware
    // with a real wear budget — so a block's back-off path would be untestable.
    let mut host = host();
    host.capabilities().refuse(Throttle::Throttled);

    let error = host.fire_timer(1).expect_err("the put is refused");

    assert_eq!(error.host_code(), Some(ErrorCode::Throttled));
    // The tick still happened; only the persistence failed. That distinction is what a
    // block author is testing.
    assert_eq!(host.block().ticks, 1);
}

#[test]
fn a_gpio_edge_is_scripted_from_the_host_side() {
    let mut host = host();
    host.fire_gpio(2, PinLevel::High).expect("edge");
    assert_eq!(host.block().last_level, Some(1));
    host.fire_gpio(2, PinLevel::Low).expect("edge");
    assert_eq!(host.block().last_level, Some(0));
}

#[test]
fn an_http_completion_distinguishes_transport_from_status() {
    // ABI §7.6: below zero is a transport error, at or above zero is the HTTP status. A
    // block retries differently for each, so a host that could only script one would make
    // half that logic untestable.
    let mut host = host();

    host.complete_http(9, -1, &[]).expect("transport failure");
    assert_eq!(host.block().last_status, Some(-1));

    host.complete_http(9, 503, &[]).expect("a server answered");
    assert_eq!(host.block().last_status, Some(503));
}

#[test]
fn a_blocks_reported_error_detail_reaches_the_host() {
    // ABI §7.0's `error` and §8: a non-zero callback return carries structured detail, and
    // a host logs and counts it. A test asserting only on the `Err` would miss what the
    // operator actually sees.
    let mut host = host();
    host.capabilities().refuse(Throttle::Io);

    host.fire_timer(1).expect_err("the put fails");

    let reported = host.reported_errors();
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(reported[0].contains("state_put"), "{reported:?}");
    assert!(reported[0].contains("ERR_IO"), "{reported:?}");
}
