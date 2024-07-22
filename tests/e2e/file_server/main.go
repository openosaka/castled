package main

import (
	"fmt"
	"io"
	"log"
	"net/http"
	"os"

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

	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) {
		dest := r.URL.Query().Get("dest")
		fw, err := os.OpenFile(dest, os.O_CREATE|os.O_WRONLY, 0644)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		defer fw.Close()

		fr, _, err := r.FormFile("file")
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		defer fw.Close()

		n, err := io.Copy(fw, fr)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}

		r.Body.Close()
		log.Println("upload", dest, n)
		fmt.Fprintf(w, "%d bytes", n)
	})

	server := &http.Server{Addr: fmt.Sprintf("%s:%d", host, port), Handler: mux}
	println(server.Addr)
	panic(server.ListenAndServe())
}
