extends Enemy

func _process(delta):
	move_by(2)
	hurt()
	queue_free()

class Minion extends Enemy:
	func tick():
		self.move_by(1)
