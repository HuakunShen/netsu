package netsu

import "fmt"

type SpeedTest interface {
	Start() error
	Stop()
}

type SpeedTestClient interface {
	Start() (*SpeedTestResult, error)
	Stop()
}

func StartServer(options SpeedTestOptions) (SpeedTest, error) {
	switch options.Protocol {
	case TCP:
		return NewTCPServer(options)
	case UDP:
		// TODO: Implement UDP server
		return nil, fmt.Errorf("UDP server not implemented yet")
	case WebSocket:
		// TODO: Implement WebSocket server
		return nil, fmt.Errorf("WebSocket server not implemented yet")
	default:
		return nil, fmt.Errorf("unsupported protocol: %s", options.Protocol)
	}
}

func StartClient(host string, options SpeedTestOptions) (SpeedTestClient, error) {
	switch options.Protocol {
	case TCP:
		return NewTCPClient(host, options)
	case UDP:
		// TODO: Implement UDP client
		return nil, fmt.Errorf("UDP client not implemented yet")
	case WebSocket:
		// TODO: Implement WebSocket client
		return nil, fmt.Errorf("WebSocket client not implemented yet")
	default:
		return nil, fmt.Errorf("unsupported protocol: %s", options.Protocol)
	}
}
