class_name Enemy
extends "res://actor.gd"

func _process(delta):
	move_by(1)
	self.hurt()

func hurt():
	super.hurt()
