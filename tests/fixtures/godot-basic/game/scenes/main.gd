extends Node2D

const PlayerScene := preload("res://scenes/player.tscn")
const Enemy := preload("enemy.gd")

var player: Player
var level_dir := "res://levels/"


func _ready() -> void:
	Events.score_changed.connect(_on_score)
	var data = load(level_dir + "one.tres")
	var broken = load("res://scenes/nope.tscn")
	var stats: Stats = load("res://resources/stats.tres")
	var dup: Dup
	var wrong_case = load("res://Scenes/Player.tscn")


func _on_player_hit() -> void:
	Events.score += 1


func _on_score(_value: int) -> void:
	pass
