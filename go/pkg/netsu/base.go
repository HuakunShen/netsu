package netsu

import (
	"time"
)

type SpeedTestBase struct {
	BytesTransferred int64
	StartTime        time.Time
	Options          SpeedTestOptions
}

func NewSpeedTestBase(options SpeedTestOptions) SpeedTestBase {
	// Set default values if not provided
	if options.Duration == 0 {
		options.Duration = 10000
	}
	if options.ChunkSize == 0 {
		options.ChunkSize = 1024 * 1024
	}
	if options.Port == 0 {
		options.Port = 5201
	}
	if options.OnProgress == nil {
		options.OnProgress = func(float64) {}
	}

	return SpeedTestBase{
		Options: options,
	}
}

func (b *SpeedTestBase) CalculateSpeed(bytes int64, durationMs int64) float64 {
	return float64(bytes*8) / (1000000 * float64(durationMs) / 1000)
}

func (b *SpeedTestBase) CreateChunk() []byte {
	chunk := make([]byte, b.Options.ChunkSize)
	for i := range chunk {
		chunk[i] = 'x'
	}
	return chunk
}

func (b *SpeedTestBase) ReportProgress() {
	elapsed := time.Since(b.StartTime).Milliseconds()
	speed := b.CalculateSpeed(b.BytesTransferred, elapsed)
	b.Options.OnProgress(speed)
}

func (b *SpeedTestBase) GetResult() SpeedTestResult {
	duration := time.Since(b.StartTime).Seconds()
	return SpeedTestResult{
		BytesTransferred: b.BytesTransferred,
		Duration:         duration,
		Speed:            b.CalculateSpeed(b.BytesTransferred, int64(duration*1000)),
		Protocol:         b.Options.Protocol,
		TestType:         b.Options.TestType,
	}
}
