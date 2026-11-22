package main

type Base struct{}

func (b *Base) Log(msg string) {}

type Logger interface {
	Log(msg string)
}
