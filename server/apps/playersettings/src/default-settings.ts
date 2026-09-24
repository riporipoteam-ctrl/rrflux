/**
 * Default player settings seeded on a player's first read. The first three are ported
 * verbatim from the reference's `PlayerSettingsController.GetPlayerSettings`. Ordered;
 * written to KV the first time a player has no stored settings.
 *
 * The `Recroom.AccountCreation.*` keys are the client's own account-creation state
 * flags (found in the client's metadata: `HasStarted`, `HasChosenUsername`,
 * `HasCreatedPassword`, `HasFinished`). The reference (and our previous defaults)
 * left these absent, and at runtime the client skipped its username/password
 * screens after avatar setup. Seeding `HasStarted=true` with the two gate flags
 * false is the hypothesis for making a fresh account enter the full flow:
 * choose username → create password → Code of Conduct. Unproven at runtime;
 * verify with a fresh-account game run before treating this as the mechanism.
 * Existing players are unaffected (defaults seed only on first read).
 */
export const DEFAULT_SETTINGS: Array<{ Key: string; Value: string }> = [
	{ Key: 'Recroom.OOBE', Value: '77' },
	{ Key: 'TUTORIAL_COMPLETE_MASK', Value: '11' },
	{ Key: 'FIRST_TIME_IN_FLAGS', Value: '0' },
	{ Key: 'Recroom.AccountCreation.HasStarted', Value: 'true' },
	{ Key: 'Recroom.AccountCreation.HasChosenUsername', Value: 'false' },
	{ Key: 'Recroom.AccountCreation.HasCreatedPassword', Value: 'false' },
]
