package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/HuakunShen/netsu/go/pkg/netsu"
)

func main() {
	mode := flag.String("mode", "", "server or client")
	protocol := flag.String("protocol", "tcp", "tcp, udp, or websocket")
	testType := flag.String("type", "download", "upload or download")
	port := flag.Int("port", 5201, "port number")
	duration := flag.Int("duration", 10000, "test duration in milliseconds")
	host := flag.String("host", "localhost", "server host (client mode only)")

	flag.Parse()

	if *mode == "" {
		fmt.Println("Please specify mode: -mode server|client")
		os.Exit(1)
	}

	options := netsu.SpeedTestOptions{
		Protocol: netsu.Protocol(*protocol),
		TestType: netsu.TestType(*testType),
		Port:     *port,
		Duration: *duration,
		OnProgress: func(speed float64) {
			fmt.Printf("\rCurrent speed: %.2f Mbps", speed)
		},
	}

	if *mode == "server" {
		server, err := netsu.StartServer(options)
		if err != nil {
			fmt.Printf("Failed to start server: %v\n", err)
			os.Exit(1)
		}

		// Start the server
		if err := server.Start(); err != nil {
			fmt.Printf("Failed to start server: %v\n", err)
			os.Exit(1)
		}

		fmt.Printf("Server started on port %d\n", *port)

		// Handle graceful shutdown
		sigChan := make(chan os.Signal, 1)
		signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

		// Wait for interrupt signal
		<-sigChan
		fmt.Println("\nShutting down server...")
		server.Stop()

	} else if *mode == "client" {
		client, err := netsu.StartClient(*host, options)
		if err != nil {
			fmt.Printf("Failed to start client: %v\n", err)
			os.Exit(1)
		}

		result, err := client.Start()
		if err != nil {
			fmt.Printf("Test failed: %v\n", err)
			os.Exit(1)
		}

		fmt.Printf("\n\nTest Results:\n")
		fmt.Printf("Protocol: %s\n", result.Protocol)
		fmt.Printf("Test type: %s\n", result.TestType)
		fmt.Printf("Bytes transferred: %d\n", result.BytesTransferred)
		fmt.Printf("Duration: %.2f seconds\n", result.Duration)
		fmt.Printf("Average speed: %.2f Mbps\n", result.Speed)
	}
}
