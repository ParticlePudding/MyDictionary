fn main() {
    // build.rs はホスト上で動くため、cfg ではなくターゲットOSを実行時に見る
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // アイコンやマニフェストを変えたときにも作り直す
        for f in ["app.rc", "app.manifest", "assets/app.ico"] {
            println!("cargo:rerun-if-changed={f}");
        }
        embed_resource::compile("app.rc", embed_resource::NONE);
    }
}
