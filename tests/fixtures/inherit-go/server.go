package main

import "sync"

type Server struct {
	Base
	sync.Mutex
	name string
}

func (s *Server) Run() {
	s.Log("run")
	s.Unlock()
}

func main() {
	s := &Server{}
	s.Run()
}
