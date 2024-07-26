package main

import (
	"fmt"
	"log"
	"net"
	"os"

	"github.com/spf13/pflag"
)

var (
	port int
	host string
)

func main() {
	pflag.IntVar(&port, "port", 12345, "")
	pflag.StringVar(&host, "host", "0.0.0.0", "")
	pflag.Parse()

	// Listen for incoming UDP packets on port 12345
	addr := net.UDPAddr{
		Port: port,
		IP:   net.ParseIP(host),
	}
	conn, err := net.ListenUDP("udp", &addr)
	if err != nil {
		fmt.Println("Error listening:", err)
		os.Exit(1)
	}
	defer conn.Close()
	log.Printf("UDP server listening on port %d", port)

	buffer := make([]byte, 1024)

	for {
		n, clientAddr, err := conn.ReadFromUDP(buffer)
		if err != nil {
			fmt.Println("Error reading from UDP:", err)
			continue
		}

		message := string(buffer[:n])
		log.Printf("Received message from %s: %s\n", clientAddr, message)
	}
}
