//! Point-stream stabilization maths for the Stroke Lab integration.
//!
//! The incremental laws that steady a pointer path before it becomes dabs:
//! position modes (pulled string, moving average, exponential, inertia),
//! the pressure chain (curve → window law → speed scale) and pose smoothing.
//! Behaviour studied clean-room (facts only); the maths here is original.
//!
//! M2 ports the laws here; the host `paint::dynamics::Smoother` mirrors them
//! stage for stage (see `docs/stroke-lab-todo.md`), pinned by the parity
//! tests in `paint` (`stroke_parity`) and the recorded-host fixture vectors
//! in `position::tests::host_stroke_fixture_parity`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

extern crate alloc;

pub mod orientation;
pub mod position;
pub mod pressure;
pub mod ruler;
pub mod script;
pub mod waves;
