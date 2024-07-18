package main

import (
	"io"
	"net/http"
	"strings"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {})
	mux.HandleFunc("/ready", func(w http.ResponseWriter, r *http.Request) {})

	mux.HandleFunc("/crawler/proxy-request", func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseForm(); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}

		requestTo := r.FormValue("requestTo")
		body := r.FormValue("body")

		println("requestTo", requestTo)
		println("body", body)

		proxyReq, err := http.NewRequest(r.Method, requestTo, strings.NewReader(body))
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		for k, v := range r.Header {
			proxyReq.Header.Set(k, v[0])
		}
		proxyReq.Host = r.Host

		resp, err := http.DefaultClient.Do(proxyReq)
		if err != nil {
			println("err1", err.Error())
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}

		b, err := io.ReadAll(resp.Body)
		if err != nil {
			println("err2", err.Error())
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.WriteHeader(resp.StatusCode)
		for k, v := range resp.Header {
			w.Header().Add(k, v[0])
		}
		w.Write(b)
	})

	http.ListenAndServe(":8080", mux)
}
