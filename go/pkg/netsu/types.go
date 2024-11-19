package netsu

type Protocol string
type TestType string

const (
	TCP       Protocol = "tcp"
	UDP       Protocol = "udp"
	WebSocket Protocol = "websocket"

	Upload   TestType = "upload"
	Download TestType = "download"
)

type SpeedTestOptions struct {
	Duration   int      `json:"duration"`  // Test duration in milliseconds
	ChunkSize  int      `json:"chunkSize"` // Chunk size in bytes
	Port       int      `json:"port"`      // Port number
	Protocol   Protocol `json:"protocol"`  // Network protocol
	TestType   TestType `json:"testType"`  // Test type
	OnProgress func(float64)
}

type SpeedTestResult struct {
	BytesTransferred int64    `json:"bytesTransferred"`
	Duration         float64  `json:"duration"`
	Speed            float64  `json:"speed"`
	Protocol         Protocol `json:"protocol"`
	TestType         TestType `json:"testType"`
}

type TestMessage struct {
	Type     string   `json:"type"`
	TestType TestType `json:"testType"`
}
