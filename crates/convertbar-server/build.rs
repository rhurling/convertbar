fn main() {
    // dist-web is produced by `npm run build:web`; CI and fresh checkouts don't have it.
    // rust-embed's derive requires the folder to EXIST — an empty dir embeds nothing and
    // the server serves API-only, which is exactly right for tests.
    std::fs::create_dir_all("../../dist-web").expect("create dist-web placeholder");
    println!("cargo:rerun-if-changed=../../dist-web");
}
