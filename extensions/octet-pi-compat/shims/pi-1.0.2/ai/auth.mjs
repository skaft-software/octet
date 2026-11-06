
export function envApiKeyAuth(name, envVars) {
    return {
        name,
        login: async (interaction) => {
            interaction.signal.throwIfAborted();
            const key = await interaction.prompt({ type: "secret", message: `Enter ${name}` });
            interaction.signal.throwIfAborted();
            return { type: "api_key", key };
        },
        resolve: async ({ ctx, credential, signal }) => {
            signal.throwIfAborted();
            if (credential?.key) {
                return { auth: { apiKey: credential.key }, env: credential.env, source: "stored credential" };
            }
            for (const envVar of envVars) {
                const value = await ctx.env(envVar);
                signal.throwIfAborted();
                if (value)
                    return { auth: { apiKey: value }, source: envVar };
            }
            return undefined;
        },
    };
}

export function lazyOAuth(input) {
    let promise;
    const loaded = () => {
        promise ??= input.load();
        return promise;
    };
    return {
        name: input.name,
        isSubscription: input.isSubscription,
        loginLabel: input.loginLabel,
        login: async (interaction, options) => (await loaded()).login(interaction, options),
        refresh: async (credential, signal) => (await loaded()).refresh(credential, signal),
        toAuth: async (credential) => (await loaded()).toAuth(credential),
    };
}
//# sourceMappingURL=helpers.js.map