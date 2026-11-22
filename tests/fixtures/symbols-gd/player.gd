extends Node
class_name Player

const Bullet = preload("res://bullet.gd")
const SPEED = 3

func _ready():
	move_by(1)
	self.move_by(1)
	var b = Bullet.new()
	b.fire_now()
	Events.emit_score(1)

func move_by(x):
	position_x += x * SPEED

var position_x = 0

class Hitbox:
	func shrink():
		pass
