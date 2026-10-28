/** RipoBot's configured default text model; never expose its token to clients. */
export async function replyFromRipoBot(
	token: string,
	messages: Array<{ role: string; content: string }>,
	fetcher: typeof fetch = fetch
): Promise<string | null> {
	try {
		const response = await fetcher('https://router.huggingface.co/v1/chat/completions', {
			method: 'POST',
			headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
			body: JSON.stringify({
				model: 'meta-llama/Llama-3.1-8B-Instruct', messages, max_tokens: 256, temperature: 0.8,
			}),
			signal: AbortSignal.timeout(30_000),
		})
		if (!response.ok) return null
		const data = (await response.json()) as { choices?: Array<{ message?: { content?: string } }> }
		return data.choices?.[0]?.message?.content?.trim() || null
	} catch {
		return null
	}
}
