#![forbid(unsafe_code)]

fn main() -> bevy::app::AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [command, rest @ ..] = args.as_slice()
        && command == "run-scene"
    {
        return jackdaw_grass_preview::run_scene(rest);
    }
    jackdaw::launch::run(jackdaw_grass_preview::install_preview)
}
