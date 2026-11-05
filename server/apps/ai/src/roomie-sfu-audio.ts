import { createRoomieProviders } from './roomie-providers'
import type { RoomieMessage } from './roomie-realtime'

/** Decode the payload field from the SFU WebSocket adapter's protobuf Packet. */
export function sfuPacketPayload(packet: Uint8Array): Uint8Array {
	let i = 0
	const varint = (): number => {
		let n = 0, shift = 0
		while (i < packet.length && shift <= 28) {
			const byte = packet[i++]
			n += (byte & 127) * 2 ** shift
			if (!(byte & 128)) return n
			shift += 7
		}
		throw new Error('Invalid packet')
	}
	while (i < packet.length) {
		const tag = varint(), field = tag >>> 3, wire = tag & 7
		if (!field) throw new Error('Invalid packet')
		if (wire === 0) { varint(); continue }
		if (wire !== 2) throw new Error('Unsupported packet field')
		const length = varint(), end = i + length
		if (end > packet.length) throw new Error('Truncated packet')
		if (field === 5) return packet.slice(i, end)
		i = end
	}
	throw new Error('Missing audio payload')
}

/** Convert 48 kHz stereo PCM16 from the SFU to 24 kHz mono for RipoBot. */
export function sfuAudioToMono(pcm: Uint8Array): Uint8Array {
	if (pcm.byteLength % 8) throw new Error('Incomplete audio frame')
	const input = new DataView(pcm.buffer, pcm.byteOffset, pcm.byteLength)
	const output = new Uint8Array(pcm.byteLength / 4)
	const view = new DataView(output.buffer)
	for (let i = 0, j = 0; i < pcm.byteLength; i += 8, j += 2) {
		const sum = input.getInt16(i, true) + input.getInt16(i + 2, true) + input.getInt16(i + 4, true) + input.getInt16(i + 6, true)
		view.setInt16(j, Math.round(sum / 4), true)
	}
	return output
}

/** Convert 24 kHz mono generated voice to 48 kHz stereo SFU input. */
export function monoToSfuAudio(pcm: Uint8Array): Uint8Array {
	if (pcm.byteLength % 2) throw new Error('Incomplete audio sample')
	const input = new DataView(pcm.buffer, pcm.byteOffset, pcm.byteLength)
	const output = new Uint8Array(pcm.byteLength * 4)
	const view = new DataView(output.buffer)
	for (let i = 0, j = 0; i < pcm.byteLength; i += 2, j += 8) {
		const sample = input.getInt16(i, true)
		for (let k = 0; k < 8; k += 2) view.setInt16(j + k, sample, true)
	}
	return output
}

/** Packet field 5 contains the PCM data; split output into messages below 32 KB. */
export function sfuAudioPackets(pcm: Uint8Array): Uint8Array[] {
	if (pcm.length % 4) throw new Error('Incomplete stereo sample')
	const packets: Uint8Array[] = []
	for (let offset = 0; offset < pcm.length; offset += 16_000) {
		const chunk = pcm.subarray(offset, Math.min(offset + 16_000, pcm.length))
		let length = chunk.length
		const header = [42]
		do { const part = length & 127; length >>>= 7; header.push(part | (length ? 128 : 0)) } while (length)
		const packet = new Uint8Array(header.length + chunk.length)
		packet.set(header); packet.set(chunk, header.length); packets.push(packet)
	}
	return packets
}

/** One completed voice turn, using the same private RipoBot services as the socket bridge. */
export async function processSfuVoiceTurn(pcm: Uint8Array, options: Parameters<typeof createRoomieProviders>[0], history: RoomieMessage[] = []): Promise<{ transcript: string; reply: string; packets: Uint8Array[] } | null> {
	if (!pcm.length || pcm.length > 5_760_000) return null
	const providers = createRoomieProviders(options)
	const transcript = await providers.transcribe(sfuAudioToMono(pcm))
	if (!transcript) return null
	const reply = await providers.reply([{ role: 'system', content: 'You are Roomie, a friendly, concise game companion.' }, ...history.slice(-10), { role: 'user', content: transcript }])
	if (!reply) return null
	const voice = await providers.synthesize(reply)
	if (!voice) return null
	return { transcript, reply, packets: sfuAudioPackets(monoToSfuAudio(voice)) }
}
