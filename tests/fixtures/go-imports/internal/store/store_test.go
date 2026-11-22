package store_test

import (
	"testing"

	"example.com/app/internal/store"
)

func TestOpen(t *testing.T) { _ = store.OpenForTest() }
