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
		// accept multipart form like
		// file1=@/path/to/large_file1.txt;filename=/tmp/dest_large_file1.txt
		// file2=@/path/to/large_file2.txt;filename=/tmp/dest_large_file2.txt
		// save the file to the filename
		if err := r.ParseMultipartForm(200 * (2 << 20)); err != nil { // 200MB
			http.Error(w, "Unable to parse multipart form", http.StatusInternalServerError)
			return
		}
		for _, headers := range r.MultipartForm.File {
			for _, header := range headers {
				file, err := header.Open()
				if err != nil {
					http.Error(w, "Unable to open file", http.StatusInternalServerError)
					return
				}
				defer file.Close()

				// Create destination file
				dst, err := os.Create("/tmp/" + header.Filename)
				if err != nil {
					http.Error(w, "Unable to create destination file", http.StatusInternalServerError)
					return
				}

				n, err := io.Copy(dst, file)
				if err != nil {
					http.Error(w, "Unable to copy file content", http.StatusInternalServerError)
					return
				}
				dst.Close()
				log.Printf("uploaded %s with size %d", header.Filename, n)
			}
		}
		fmt.Fprint(w, "OK")
	})

	server := &http.Server{Addr: fmt.Sprintf("%s:%d", host, port), Handler: mux}
	println(server.Addr)
	panic(server.ListenAndServe())
}
