package netsu

import (
	"encoding/json"
	"fmt"
	"net"
	"time"
)

type TCPServer struct {
	SpeedTestBase
	listener net.Listener
}

func NewTCPServer(options SpeedTestOptions) (*TCPServer, error) {
	base := NewSpeedTestBase(options)
	return &TCPServer{SpeedTestBase: base}, nil
}

func (s *TCPServer) Start() error {
	var err error
	s.listener, err = net.Listen("tcp", fmt.Sprintf(":%d", s.Options.Port))
	if err != nil {
		return err
	}

	fmt.Printf("TCP server listening on port %d\n", s.Options.Port)

	go func() {
		for {
			conn, err := s.listener.Accept()
			if err != nil {
				return
			}
			go s.handleConnection(conn)
		}
	}()

	return nil
}

func (s *TCPServer) handleConnection(conn net.Conn) {
	defer conn.Close()
	fmt.Println("Client connected")

	var testType TestType
	var startTime time.Time
	var bytesTransferred int64

	// Read start message
	decoder := json.NewDecoder(conn)
	var msg TestMessage
	if err := decoder.Decode(&msg); err != nil {
		fmt.Println("Invalid start message:", err)
		return
	}

	testType = msg.TestType
	startTime = time.Now()

	if testType == Download {
		s.startDownloadTest(conn, &bytesTransferred, startTime)
	} else {
		buffer := make([]byte, s.Options.ChunkSize)
		for {
			n, err := conn.Read(buffer)
			if err != nil {
				break
			}
			bytesTransferred += int64(n)
			elapsed := time.Since(startTime).Milliseconds()
			speed := s.CalculateSpeed(bytesTransferred, elapsed)
			s.Options.OnProgress(speed)
		}
	}
}

func (s *TCPServer) startDownloadTest(conn net.Conn, bytesTransferred *int64, startTime time.Time) {
	chunk := s.CreateChunk()
	for {
		if time.Since(startTime).Milliseconds() >= int64(s.Options.Duration) {
			break
		}

		n, err := conn.Write(chunk)
		if err != nil {
			break
		}
		*bytesTransferred += int64(n)
		speed := s.CalculateSpeed(*bytesTransferred, time.Since(startTime).Milliseconds())
		s.Options.OnProgress(speed)
	}
}

func (s *TCPServer) Stop() {
	if s.listener != nil {
		s.listener.Close()
	}
}

type TCPClient struct {
	SpeedTestBase
	host string
	conn net.Conn
}

func NewTCPClient(host string, options SpeedTestOptions) (*TCPClient, error) {
	base := NewSpeedTestBase(options)
	return &TCPClient{
		SpeedTestBase: base,
		host:          host,
	}, nil
}

func (c *TCPClient) Start() (*SpeedTestResult, error) {
	var err error
	c.conn, err = net.Dial("tcp", fmt.Sprintf("%s:%d", c.host, c.Options.Port))
	if err != nil {
		return nil, err
	}
	defer c.conn.Close()

	c.StartTime = time.Now()

	// Send start message
	startMsg := TestMessage{
		Type:     "start",
		TestType: c.Options.TestType,
	}
	encoder := json.NewEncoder(c.conn)
	if err := encoder.Encode(startMsg); err != nil {
		return nil, err
	}

	done := make(chan struct{})
	go func() {
		if c.Options.TestType == Upload {
			c.startUpload()
		} else {
			c.handleDownload()
		}
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(time.Duration(c.Options.Duration) * time.Millisecond):
	}

	result := c.GetResult()
	return &result, nil
}

func (c *TCPClient) startUpload() {
	chunk := c.CreateChunk()
	for time.Since(c.StartTime).Milliseconds() < int64(c.Options.Duration) {
		n, err := c.conn.Write(chunk)
		if err != nil {
			break
		}
		c.BytesTransferred += int64(n)
		c.ReportProgress()
	}
}

func (c *TCPClient) handleDownload() {
	buffer := make([]byte, c.Options.ChunkSize)
	for time.Since(c.StartTime).Milliseconds() < int64(c.Options.Duration) {
		n, err := c.conn.Read(buffer)
		if err != nil {
			break
		}
		c.BytesTransferred += int64(n)
		c.ReportProgress()
	}
}

func (c *TCPClient) Stop() {
	if c.conn != nil {
		c.conn.Close()
	}
}
