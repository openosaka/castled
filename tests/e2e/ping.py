#!/usr/bin/env python3

from http.server import BaseHTTPRequestHandler, HTTPServer
import urllib.parse as urlparse
import sys

class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def handle_request(self):
        parsed_path = urlparse.urlparse(self.path)
        query_params = urlparse.parse_qs(parsed_path.query)
        query_value = query_params.get('query', [''])[0]

        if parsed_path.path == '/ping':
            response = f'pong={query_value}'
            self.wfile.write(response.encode())
        else:
            self.send_response(404)
            self.wfile.write(b'Not Found')
    def do_GET(self):
        self.handle_request()
    def do_POST(self):
        self.handle_request()

def run(server_class=HTTPServer, handler_class=SimpleHTTPRequestHandler, port=12345):
    server_address = ('', port)
    httpd = server_class(server_address, handler_class)
    print(f'Starting httpd server on port {port}...')
    httpd.serve_forever()

if __name__ == '__main__':
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <port>")
        sys.exit(1)

    port = int(sys.argv[1])
    run(port=port)
