# Builder points

In a scene with a BuildingRecipe, select its owner (optional if there is only one), then choose **Tools → Edit Builder Points**. Drag a point, use X/Y/Z to choose its local axis, hold Shift to snap the delta to 1/8, and press Escape to leave the tool. Release commits one document field. The preview follows the document through undo and redo.

Place `builder.json` in the editor's local configuration directory (beside `extensions.json`):

```json
{
  "compiler": "D:/tools/compile_building_recipe.exe",
  "mesh_dir": "preview-meshes"
}
```

Supply a native Builder recipe compiler supporting `--from-bsn FILE`. It receives a temporary, single-recipe BSN document and writes one instance per line (source, variant ID, material ID, translation XYZ, quaternion XYZW and scale XYZ). The compiler must match the project's recipe format. Compilation starts after the user activates the tool. The optional mesh directory is relative to the project's assets and contains `<variant>.glb`; the preview uses the first primitive of the first mesh, with instance bounds as fallback. The launcher may override each setting with `JACKDAW_BUILDER_COMPILER` or `JACKDAW_BUILDER_MESH_DIR`.
