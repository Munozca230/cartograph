extends SceneTree

const TESTS := [
	"res://scripts/dup_a.gd",
	"res://scripts/dup_b.gd",
]


func _init() -> void:
	for path in TESTS:
		var script: GDScript = load(path)
	var source := FileAccess.get_file_as_string("res://scenes/main.gd")
	quit()
