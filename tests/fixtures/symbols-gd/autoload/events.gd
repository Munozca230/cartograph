extends Node

signal scored(value)

func emit_score(value):
	scored.emit(value)
