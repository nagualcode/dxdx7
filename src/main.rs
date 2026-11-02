//! Standalone mode: run the plugin as a desktop app via `cargo truce run`,
//! no DAW needed. Only compiled with the `standalone` feature.

use dxdx7::Plugin;

fn main() {
    truce_standalone::run::<Plugin>();
}
