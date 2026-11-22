class_name Player
extends Actor

var weapon: Weapon


func _on_enemy_died() -> void:
	Events.score += 10
