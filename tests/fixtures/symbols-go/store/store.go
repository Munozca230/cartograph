package store

type Store struct {
	items []string
}

type Saver interface {
	Save()
}

func New() *Store {
	return &Store{}
}

func (s *Store) Save() {
	s.write()
}

func (s *Store) write() {
	_ = len(s.items)
}
