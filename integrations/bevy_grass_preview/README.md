# GPU grass preview

This launcher adds a live `bevy_grass` preview to Jackdaw's terrain editor.
It expects the `bevy_editor` and `bevy_grass` source checkouts next to each
other. Build it from the editor checkout.

```powershell
cargo run --locked --manifest-path integrations/bevy_grass_preview/Cargo.toml
```

Open a terrain, select a layer in **Terrain → Detail**, then run **Toggle GPU
Grass Preview** from the command palette. In **Paint → Detail**, the density
brush, undo and redo update the preview. **GPU Grass Preview Status** reports
the selected field's readiness. The remote operator
`terrain.grass_preview` also accepts an explicit `enabled` boolean.

In the Detail brush toolbar, choose **Density**, **Grass Height**, or
**Grass Color**. Height is a percentage of the layer's authored blade length.
Color is an RGB multiplier over the layer's existing palette. Hold Ctrl or
enable **Restore** to paint full height or neutral white. The editor creates
an appearance channel on first use and stores it in the terrain sidecar.

The preview follows the selected terrain and layer in the main 3D viewport.
The terrain document owns the height and density data, and the scene sidecar
stores edits. The preview field and its GPU resources are rebuilt from that
data when a scene is loaded. The original layer returns when the preview is
disabled.

The integration compiles the existing renderer sources with the editor's
locked Bevy Git revision and uses the compute path. The standalone
`bevy_grass` workspace continues to use its released Bevy dependency. The
editor camera keeps its FXAA and transparency settings.

## Grass assets and mixed ground cover

Export a `.grass.json` file and its atlas PNG from the Lab. Put both under the
project's assets folder, then set **Grass asset** on the Detail layer to the
asset-relative JSON path. The grass asset supplies the palette, wind and LOD
settings. The layer still controls where grass grows and how dense it is.

Enable GPU Grass Preview and select **Grass Species** in the brush toolbar.
The picker shows the names from the loaded asset. Ctrl or **Restore** returns
the brushed cells to the first grass type. Density, length and tint keep their
own channels. The species channels use the order of the asset's palette, so
add new types at the end of an existing palette to preserve painted indices.

To play back a saved scene in a Bevy window using the game-side projection:

```powershell
jackdaw_grass_preview.exe run-scene <assets-directory> <relative-scene.bsn>
```

A game can install `TerrainGrassPlugin` after `JackdawPlugin` and
`ProceduralGrassPlugin` and mark its main view with `GrassCamera`.

## Preview while painting

In Paint → Detail, **Overlay** switches between the density, height, color
and species maps. The brush target has its own selector. The species overlay
colors the first three palette slots green, pink and amber.

Enable **Toggle GPU Grass Preview** to select **LOD · Auto**, **Near**,
**Middle** or **Far** for the active field. The lock applies in Detail paint
mode. Leaving that mode returns to the camera-based LOD. The coverage budgets
and field visibility still apply to the preview.

## Rain in a loaded scene

Press **R** in the `run-scene` window to start or stop rain. The scene runtime
drives the shared `GrassWeather` resource, which the grass renderer uses for
wind, darkening, roughness and reflectance.

## Grass beyond the last tuft

The terrain's material takes over the grass color through the tuft fade.
It samples the field's density and blade-length masks, so roads and changes
made during play appear in the distant surface. The horizon follows the
first ready GPU Detail layer of each terrain. Use that layer's species map
for mixed colors.

The editor uses the active GPU preview layer. In a game, the terrain bridge
attaches `GrassRuntimeGround` to each bound field. Game actions can send
`GrassTerrainEdit`, query that field and save its sparse snapshot.
`GRASS_SCENE_HORIZON=off` switches off the terrain blend in the standalone
scene viewer for comparison.
