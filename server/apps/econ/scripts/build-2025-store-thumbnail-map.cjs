const fs = require('node:fs')
const path = require('node:path')

const root = path.resolve(__dirname, '..')
const cai = JSON.parse(fs.readFileSync(path.join(root, 'static/db/2025-1-cai.json'), 'utf8'))
const storefront = JSON.parse(
	fs.readFileSync(path.join(root, 'static/storefronts/sf3-2025.json'), 'utf8')
)

function thumbnail(item) {
	const filename = item.ThumbnailImageFilename ?? item.CurrentSaves?.[0]?.ThumbnailFileName
	if (typeof filename !== 'string' || !/^[a-z0-9_-]+\.png$/i.test(filename)) return null
	return `avatar/${filename}`
}

const caiById = new Map()
const caiByName = new Map()
for (const item of cai) {
	const id = item.CustomAvatarItemId?.toLowerCase()
	if (id) {
		if (caiById.has(id)) throw new Error(`Duplicate CAI item id: ${id}`)
		caiById.set(id, item)
	}
	if (item.IsRecRoomApproved && item.CreatorAccountId === 1 && item.Name) {
		const name = item.Name.trim().toLowerCase()
		caiByName.set(name, [...(caiByName.get(name) ?? []), item])
	}
}

const byId = {}
const namesWithoutAvatarInfo = new Set()
for (const { GiftDrop: drop } of storefront.StoreItems) {
	const id = drop.AvatarItemInfo?.DownloadableAvatarItemId?.toLowerCase()
	if (id) {
		const item = caiById.get(id)
		const image = item && thumbnail(item)
		if (image) byId[id] = image
	} else if (drop.FriendlyName) {
		namesWithoutAvatarInfo.add(drop.FriendlyName.trim().toLowerCase())
	}
}

const byName = {}
for (const name of namesWithoutAvatarInfo) {
	const matches = caiByName.get(name) ?? []
	if (matches.length !== 1) continue
	const image = thumbnail(matches[0])
	if (image) byName[name] = image
}

const output = path.join(root, 'static/db/2025-store-thumbnail-map.json')
fs.writeFileSync(output, `${JSON.stringify({ byId, byName }, null, 2)}\n`)
console.log(
	`Wrote ${Object.keys(byId).length} ID thumbnails and ${Object.keys(byName).length} unique-name thumbnails.`
)
