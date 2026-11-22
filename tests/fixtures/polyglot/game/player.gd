extends CharacterBody2D

const SPEED := 120.0

func _physics_process(_delta: float) -> void:
	velocity = Input.get_vector("left", "right", "up", "down") * SPEED
	move_and_slide()
