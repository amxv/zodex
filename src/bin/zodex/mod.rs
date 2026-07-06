include!("prelude.rs");
include!("dispatch.rs");
include!("credentials.rs");
include!("sprite_proxy.rs");
include!("github_device.rs");
include!("github_mode.rs");
include!("lifecycle.rs");
include!("process.rs");
include!("status.rs");
include!("system_tls.rs");

#[cfg(test)]
mod tests {
    include!("tests/part1.rs");
    include!("tests/part2.rs");
}
