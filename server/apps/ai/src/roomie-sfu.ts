/** Create an SFU publication for a client's single microphone track. */
export async function createRoomieSfuSession(appId: string, secret: string, sdp: string): Promise<{ sessionId: string; answer: string }> {
	if (!/^[a-f0-9]{32}$/i.test(appId) || !secret || sdp.length > 100_000 || !sdp.startsWith('v=0')) {
		throw new Error('Invalid SFU configuration or offer')
	}
	const sections = sdp.split(/\r?\nm=/)
	const audio = sections.slice(1).find(section => section.startsWith('audio ') && !section.startsWith('audio 0 '))
	const mid = audio?.match(/(?:^|\r?\n)a=mid:([^\s]+)/)?.[1]
	if (!mid || mid.length > 64) throw new Error('Offer has no active audio track')
	const base = `https://rtc.live.cloudflare.com/v1/apps/${appId}`
	const headers = { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' }
	const created = await fetch(`${base}/sessions/new`, { method: 'POST', headers })
	if (!created.ok) throw new Error('SFU session creation failed')
	const session = await created.json() as { sessionId?: string; errorCode?: string }
	if (!session.sessionId || !/^[A-Za-z0-9_-]{1,128}$/.test(session.sessionId) || session.errorCode) throw new Error('SFU session creation failed')
	const response = await fetch(`${base}/sessions/${session.sessionId}/tracks/new`, {
		method: 'POST', headers,
		body: JSON.stringify({ sessionDescription: { type: 'offer', sdp }, tracks: [{ location: 'local', mid, trackName: 'roomie-player-mic', bidirectionalMediaStream: true, kind: 'audio' }] }),
	})
	if (!response.ok) throw new Error('SFU negotiation failed')
	const result = await response.json() as { sessionDescription?: { type?: string; sdp?: string }; tracks?: Array<{ errorCode?: string }>; errorCode?: string }
	if (result.errorCode || !result.tracks?.length || result.tracks.some(track => track.errorCode) ||
		result.sessionDescription?.type !== 'answer' || !result.sessionDescription.sdp) throw new Error('SFU negotiation failed')
	return { sessionId: session.sessionId, answer: result.sessionDescription.sdp }
}

/** Attach the player's published track and a separate generated-voice publication. */
export async function createRoomieSfuAdapters(
	appId: string, secret: string, sessionId: string, socketOrigin: string, token: string
): Promise<{ voiceSessionId: string; voiceTrackName: string; adapterIds: string[] }> {
	if (!/^[a-f0-9]{32}$/i.test(appId) || !/^[A-Za-z0-9_-]{1,128}$/.test(sessionId)) throw new Error('Invalid SFU session')
	const origin = new URL(socketOrigin)
	if (origin.protocol !== 'https:' || origin.username || origin.password || origin.pathname !== '/' || origin.search || origin.hash) throw new Error('Invalid socket origin')
	const endpoint = (direction: string) => {
		const url = new URL('/roomieai/sfu/audio', origin)
		url.protocol = 'wss:'
		url.searchParams.set('direction', direction)
		url.searchParams.set('token', token)
		return url.toString()
	}
	const base = `https://rtc.live.cloudflare.com/v1/apps/${appId}/adapters/websocket`
	const headers = { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' }
	const response = await fetch(`${base}/new`, {
		method: 'POST', headers,
		body: JSON.stringify({ tracks: [
			{ location: 'remote', sessionId, trackName: 'roomie-player-mic', endpoint: endpoint('input'), outputCodec: 'pcm' },
			{ location: 'local', trackName: 'roomie-generated-voice', endpoint: endpoint('output'), inputCodec: 'pcm' },
		] }),
	})
	if (!response.ok) throw new Error('SFU adapter creation failed')
	const result = await response.json() as { tracks?: Array<{ adapterId?: string; sessionId?: string; trackName?: string; errorCode?: string }> }
	const tracks = result.tracks || []
	const input = tracks.find(track => track.trackName === 'roomie-player-mic')
	const output = tracks.find(track => track.trackName === 'roomie-generated-voice')
	if (tracks.length !== 2 || !input?.adapterId || input.errorCode || !output?.adapterId || output.errorCode || !output.sessionId) {
		const ids = tracks.flatMap(track => track.adapterId ? [{ adapterId: track.adapterId }] : [])
		if (ids.length) await fetch(`${base}/close`, { method: 'POST', headers, body: JSON.stringify({ tracks: ids }) }).catch(() => {})
		throw new Error('SFU adapter creation failed')
	}
	return { voiceSessionId: output.sessionId, voiceTrackName: 'roomie-generated-voice', adapterIds: [input.adapterId, output.adapterId] }
}

export async function closeRoomieSfuAdapters(appId: string, secret: string, adapterIds: string[]): Promise<void> {
	if (!/^[a-f0-9]{32}$/i.test(appId) || !adapterIds.length || adapterIds.length > 4) throw new Error('Invalid adapters')
	const response = await fetch(`https://rtc.live.cloudflare.com/v1/apps/${appId}/adapters/websocket/close`, {
		method: 'POST', headers: { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' },
		body: JSON.stringify({ tracks: adapterIds.map(adapterId => ({ adapterId })) }),
	})
	if (!response.ok) throw new Error('Adapter cleanup failed')
}

export async function requestRoomieVoiceTrack(appId: string, secret: string, sessionId: string, voiceSessionId: string, voiceTrackName: string): Promise<unknown> {
	if (!/^[a-f0-9]{32}$/i.test(appId) || !/^[A-Za-z0-9_-]{1,128}$/.test(sessionId) || !/^[A-Za-z0-9_-]{1,128}$/.test(voiceSessionId)) throw new Error('Invalid SFU session')
	const response = await fetch(`https://rtc.live.cloudflare.com/v1/apps/${appId}/sessions/${sessionId}/tracks/new`, {
		method: 'POST', headers: { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' },
		body: JSON.stringify({ tracks: [{ location: 'remote', sessionId: voiceSessionId, trackName: voiceTrackName, mid: '#roomie-player-mic', kind: 'audio' }] }),
	})
	if (!response.ok) throw new Error('Voice subscription failed')
	const result = await response.json() as { sessionDescription?: { type?: string; sdp?: string }; requiresImmediateRenegotiation?: boolean; tracks?: Array<{ errorCode?: string }>; errorCode?: string }
	if (result.errorCode || !result.tracks?.length || result.tracks.some(t => t.errorCode) || result.requiresImmediateRenegotiation) throw new Error('Voice subscription failed')
	return { attached: true }
}

export async function answerRoomieVoiceTrack(appId: string, secret: string, sessionId: string, sdp: string): Promise<void> {
	if (!/^[a-f0-9]{32}$/i.test(appId) || !/^[A-Za-z0-9_-]{1,128}$/.test(sessionId)) throw new Error('Invalid SFU session')
	const response = await fetch(`https://rtc.live.cloudflare.com/v1/apps/${appId}/sessions/${sessionId}/renegotiate`, {
		method: 'PUT', headers: { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' },
		body: JSON.stringify({ sessionDescription: { type: 'answer', sdp } }),
	})
	if (!response.ok) throw new Error('Voice answer failed')
	const result = await response.json() as { errorCode?: string }
	if (result.errorCode) throw new Error('Voice answer failed')
}
