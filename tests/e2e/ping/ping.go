package main

import (
	"fmt"
	"net/http"

	"github.com/spf13/pflag"
)

var (
	port int
	host string
)

func main() {
	pflag.IntVar(&port, "port", 8080, "")
	pflag.StringVar(&host, "host", "127.0.0.1", "")
	pflag.Parse()

	mux := http.NewServeMux()

	mux.HandleFunc("/ping", func(w http.ResponseWriter, r *http.Request) {
		query := r.URL.Query().Get("query")
		w.Write([]byte(fmt.Sprintf("pong=%s", query)))
	})

	server := &http.Server{Addr: fmt.Sprintf("%s:%d", host, port), Handler: mux}
	println(server.Addr)
	panic(server.ListenAndServe())
}
