#!/usr/bin/env python3

from http.server import BaseHTTPRequestHandler, HTTPServer
import socketserver
import urllib.parse as urlparse
import sys

class Ping(BaseHTTPRequestHandler):
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

def run(port=12345, host="localhost"):
    print(f"Starting server on {host}:{port}")

    server_address = (host, port)
    httpd = socketserver.TCPServer(server_address, Ping)
    httpd.serve_forever()

if __name__ == '__main__':
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <port> <host>")
        sys.exit(1)

    port = int(sys.argv[1])
    if len(sys.argv) == 3:
        host = sys.argv[2]
        run(port=port, host=host)
    else:
        run(port=port)
