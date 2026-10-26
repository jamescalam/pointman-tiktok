// The proto's messages as Rust types (buffa's), for the typed handlers: proto/tiktok/v1/tiktok.proto.
fn main() {
    pointman_extension::proto::build().expect("proto/tiktok/v1/tiktok.proto builds");
}
