package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/codemoo/hmux/internal/homeservice"
	"github.com/codemoo/hmux/internal/webgateway"
)

func serveGateway(ctx context.Context, opts webOptions) error {
	if err := webgateway.LoopbackAddress(opts.listen); err != nil {
		return err
	}
	if info, err := os.Stat(opts.assets + "/index.html"); err != nil || !info.Mode().IsRegular() {
		return errors.New("built web assets missing; run npm ci && npm run build in web/")
	}
	handler, err := webgateway.NewServer(opts.origin, opts.credentials, opts.tokenPath, os.DirFS(opts.assets))
	if err != nil {
		return err
	}
	defer handler.Close()
	transportLog, err := homeservice.OpenLog(opts.credentials + ".transport.log")
	if err != nil {
		return errors.New("gateway transport log unavailable")
	}
	defer transportLog.Close()
	transportLogger := log.New(transportLog, "", log.LstdFlags|log.LUTC)
	handler.SetTransportLog(func(event string) { transportLogger.Println(event) })
	transportLogger.Println("Gateway starting")
	srv := &http.Server{Addr: opts.listen, Handler: handler, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 15 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 8192}
	go func() {
		<-ctx.Done()
		bounded, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(bounded)
	}()
	fmt.Println("HMux web listening on", opts.listen, "behind HTTPS")
	err = srv.ListenAndServe()
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
