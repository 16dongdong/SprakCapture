package pluginsdk

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
)

// ByteArray 强制二进制字节按 ABI v2 的 JSON 整数数组编码，而不是 Go 默认的 Base64 字符串。
type ByteArray []byte

// MarshalJSON 输出 `[0..255]` 数组；该格式与 Rust 宿主的 Vec<u8> 序列化完全一致。
func (value ByteArray) MarshalJSON() ([]byte, error) {
	output := make([]byte, 0, 2+len(value)*4)
	output = append(output, '[')
	for index, item := range value {
		if index > 0 {
			output = append(output, ',')
		}
		output = strconv.AppendUint(output, uint64(item), 10)
	}
	output = append(output, ']')
	return output, nil
}

// UnmarshalJSON 严格读取 0..255 整数数组；字符串和越界数字均拒绝，避免静默改变线上字节。
func (value *ByteArray) UnmarshalJSON(encoded []byte) error {
	var numbers []uint16
	if err := json.Unmarshal(encoded, &numbers); err != nil {
		return errors.New("bytes 必须是 0..255 的整数数组")
	}
	decoded := make([]byte, len(numbers))
	for index, number := range numbers {
		if number > 255 {
			return errors.New("bytes 包含超过 255 的整数")
		}
		decoded[index] = byte(number)
	}
	*value = decoded
	return nil
}

// BinaryEvent 提供 TCP chunk、UDP datagram、WebSocket 和正文块的完整字节视图。
type BinaryEvent struct {
	Bytes       ByteArray `json:"bytes"`
	EndOfStream bool      `json:"endOfStream"`
}

// ParseBinaryEvent 从支持的阶段读取二进制负载；非二进制阶段或畸形数组明确失败。
func ParseBinaryEvent(invocation Invocation) (BinaryEvent, error) {
	switch invocation.Envelope.Stage {
	case StageTCPChunk, StageUDPDatagram, StageRequestBodyChunk, StageResponseBodyChunk, StageWebSocketFrame:
	default:
		return BinaryEvent{}, errors.New("当前阶段不包含可修改二进制负载")
	}
	var event BinaryEvent
	if err := json.Unmarshal(invocation.Envelope.Payload, &event); err != nil {
		return BinaryEvent{}, fmt.Errorf("解析二进制负载: %w", err)
	}
	return event, nil
}

// SplitPackets 按最大负载长度分包；空负载保留一个空包，零上限明确失败。
func SplitPackets(bytes []byte, maximumPacketBytes int) ([][]byte, error) {
	if maximumPacketBytes <= 0 {
		return nil, errors.New("分包大小必须大于零")
	}
	if len(bytes) == 0 {
		return [][]byte{{}}, nil
	}
	packets := make([][]byte, 0, (len(bytes)+maximumPacketBytes-1)/maximumPacketBytes)
	for start := 0; start < len(bytes); start += maximumPacketBytes {
		end := min(start+maximumPacketBytes, len(bytes))
		packets = append(packets, append([]byte(nil), bytes[start:end]...))
	}
	return packets, nil
}

// JoinPackets 按原顺序合并分片；总长度溢出 int 时在分配前失败。
func JoinPackets(packets [][]byte) ([]byte, error) {
	totalBytes := 0
	for _, packet := range packets {
		if len(packet) > int(^uint(0)>>1)-totalBytes {
			return nil, errors.New("合包总长度溢出地址空间")
		}
		totalBytes += len(packet)
	}
	joined := make([]byte, 0, totalBytes)
	for _, packet := range packets {
		joined = append(joined, packet...)
	}
	return joined, nil
}

// LengthPrefixedFrames 增量解析跨 TCP chunk 的 4 字节大端长度前缀帧。
type LengthPrefixedFrames struct {
	buffer            []byte
	maximumFrameBytes int
}

// NewLengthPrefixedFrames 创建分帧器；零或负上限没有有效协议语义。
func NewLengthPrefixedFrames(maximumFrameBytes int) (*LengthPrefixedFrames, error) {
	if maximumFrameBytes <= 0 {
		return nil, errors.New("帧大小上限必须大于零")
	}
	return &LengthPrefixedFrames{maximumFrameBytes: maximumFrameBytes}, nil
}

// Push 追加一个 TCP chunk 并返回全部完整帧；不完整尾部保留到下次调用。
//
// 声明长度超过上限时清空缓冲并失败，防止攻击性前缀永久占用连接内存。
func (frames *LengthPrefixedFrames) Push(chunk []byte) ([][]byte, error) {
	frames.buffer = append(frames.buffer, chunk...)
	completed := make([][]byte, 0)
	consumed := 0
	for len(frames.buffer)-consumed >= 4 {
		frameBytes := int(binary.BigEndian.Uint32(frames.buffer[consumed : consumed+4]))
		if frameBytes > frames.maximumFrameBytes {
			frames.buffer = nil
			return nil, fmt.Errorf("帧长度 %d 超过上限 %d", frameBytes, frames.maximumFrameBytes)
		}
		frameEnd := consumed + 4 + frameBytes
		if frameEnd > len(frames.buffer) {
			break
		}
		completed = append(completed, append([]byte(nil), frames.buffer[consumed+4:frameEnd]...))
		consumed = frameEnd
	}
	if consumed > 0 {
		frames.buffer = append(frames.buffer[:0], frames.buffer[consumed:]...)
	}
	return completed, nil
}

// Encode 用 4 字节大端长度前缀重封包单帧；超过配置或 uint32 上限时拒绝输出。
func (frames *LengthPrefixedFrames) Encode(frame []byte) ([]byte, error) {
	if len(frame) > frames.maximumFrameBytes || uint64(len(frame)) > uint64(^uint32(0)) {
		return nil, fmt.Errorf("帧长度 %d 超过上限 %d", len(frame), frames.maximumFrameBytes)
	}
	packet := make([]byte, 4+len(frame))
	binary.BigEndian.PutUint32(packet[:4], uint32(len(frame)))
	copy(packet[4:], frame)
	return packet, nil
}

// BufferedBytes 返回尚未构成完整帧的字节数，用于连接关闭完整性检查。
func (frames *LengthPrefixedFrames) BufferedBytes() int { return len(frames.buffer) }
