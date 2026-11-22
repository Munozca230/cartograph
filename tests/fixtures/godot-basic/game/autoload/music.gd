extends AudioStreamPlayer


func _ready() -> void:
	Events.score_changed.connect(_on_score)


func _on_score(_value: int) -> void:
	play()
