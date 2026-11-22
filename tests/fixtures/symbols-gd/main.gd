extends Node

func _ready():
	var p = Player.new()
	p.move_by(2)
	start_level()

func start_level():
	Events.emit_score(0)
