package recovery

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

type checkpointStub struct {
	saves int
	fail  bool
}

func (s *checkpointStub) Save(context.Context) error {
	s.saves++
	if s.fail {
		return errors.New("synthetic checkpoint failure")
	}
	return nil
}

func TestCatalogRecoveryCheckpointCadenceAndRetry(t *testing.T) {
	now := time.Unix(100, 0)
	saver := &checkpointStub{}
	fetches := 0
	fetch := catalogFetchWithRecoveryClock(func(context.Context) (model.Catalog, error) { fetches++; return model.Catalog{}, nil }, saver, func() time.Time { return now })
	for i := 0; i < 6; i++ {
		if _, err := fetch(t.Context()); err != nil {
			t.Fatal(err)
		}
		now = now.Add(5 * time.Second)
	}
	if saver.saves != 0 || fetches != 6 {
		t.Fatal("checkpoint ran per catalog/tab")
	}
	saver.fail = true
	if _, err := fetch(t.Context()); err != nil {
		t.Fatal("checkpoint failure broke catalog")
	}
	saver.fail = false
	if _, err := fetch(t.Context()); err != nil {
		t.Fatal(err)
	}
	if saver.saves != 2 {
		t.Fatal("failed checkpoint was not retried")
	}
	if _, err := fetch(t.Context()); err != nil {
		t.Fatal(err)
	}
	if saver.saves != 2 {
		t.Fatal("successful checkpoint was not throttled")
	}
}
