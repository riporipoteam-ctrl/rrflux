import { describe, expect, it, vi } from 'vitest'
import { replyFromRipoBot } from '../../ripobot'

describe('RipoBot text provider', () => {
	it('sends the conversation to the configured model and returns its reply', async () => {
		const fetcher = vi.fn(async () => Response.json({ choices: [{ message: { content: ' Hello! ' } }] }))
		const messages = [{ role: 'user', content: 'Hi' }]
		expect(await replyFromRipoBot('test-token', messages, fetcher as unknown as typeof fetch)).toBe('Hello!')
		const [url, options] = fetcher.mock.calls[0] as unknown as [string, RequestInit]
		expect(url).toBe('https://router.huggingface.co/v1/chat/completions')
		expect((options.headers as Record<string, string>).Authorization).toBe('Bearer test-token')
		expect(JSON.parse(options.body as string)).toMatchObject({ model: 'meta-llama/Llama-3.1-8B-Instruct', messages })
	})

	it('uses the configured RipoBot model without sending the Hugging Face token to the client', async () => {
		const fetcher = vi.fn(async () => Response.json({ choices: [{ message: { content: 'Hello' } }] }))
		expect(await replyFromRipoBot('test-token', [], fetcher as unknown as typeof fetch, 'echoxr/ripobot-model')).toBe('Hello')
		const [, options] = fetcher.mock.calls[0] as unknown as [string, RequestInit]
		expect(JSON.parse(options.body as string).model).toBe('echoxr/ripobot-model')
	})

	it('returns no reply on provider errors; callers do not switch to another model', async () => {
		const badResponse = vi.fn(async () => new Response('', { status: 503 }))
		const networkError = vi.fn(async () => { throw new Error('offline') })
		expect(await replyFromRipoBot('token', [], badResponse as unknown as typeof fetch)).toBeNull()
		expect(await replyFromRipoBot('token', [], networkError as unknown as typeof fetch)).toBeNull()
	})

	it('does not call any model when the Hugging Face credential is empty', async () => {
		const fetcher = vi.fn()
		expect(await replyFromRipoBot('', [], fetcher as unknown as typeof fetch)).toBeNull()
		expect(fetcher).not.toHaveBeenCalled()
	})
})
