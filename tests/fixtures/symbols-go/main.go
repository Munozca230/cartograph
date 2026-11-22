package main

import (
	"fmt"
	"net/http"

	"example.com/app/store"
)

func main() {
	s := store.New()
	s.Save()
	http.HandleFunc("/health", health)
	localHelper()
	fmt.Println("up")
}

func health(w http.ResponseWriter, r *http.Request) {}
