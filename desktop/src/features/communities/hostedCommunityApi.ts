import { invoke } from "@tauri-apps/api/core";

export const HOSTED_COMMUNITY_SUFFIX = "communities.hireshelby.com";
export const HOSTED_COMMUNITY_LIMIT = 3;
export const VALID_HOSTED_COMMUNITY_NAME = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export type AccountsAuth = {
  email?: string;
  name?: string;
  expiresAt: string;
};

export type HostedCommunityApiError = {
  code?: string;
  message?: string;
  setup_needed?: boolean;
};

export type HostedNostrIdentity = {
  npub?: string;
  pubkey_hex?: string;
};

export type HostedIdentityResponse = {
  identity?: HostedNostrIdentity;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunity = {
  id?: string;
  name?: string;
  slug?: string;
  normalized_host?: string;
  owner_pubkey?: string;
  archived_at?: string | null;
};

export type HostedCommunitiesResponse = {
  communities?: HostedCommunity[];
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityAvailabilityResponse = {
  available?: boolean;
  normalized_host?: string;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityMutationResponse = {
  community?: HostedCommunity;
  error?: HostedCommunityApiError;
  correlation_id?: string;
};

export type HostedCommunityAccount = {
  communities: HostedCommunity[];
  identity: HostedNostrIdentity | null;
};

export function hostedCommunityErrorMessage(
  error: HostedCommunityApiError | undefined,
  correlationId: string | undefined,
  fallback: string,
) {
  const messages: Record<string, string> = {
    missing_mapping:
      "Connect your HireShelby identity before creating a community.",
    invalid_name: "Use lowercase letters, numbers, and hyphens.",
    taken: "That HireShelby address is already taken.",
    limit_reached: `You've reached the limit of ${HOSTED_COMMUNITY_LIMIT} hosted communities.`,
    relay_unavailable: "Community provisioning is temporarily unavailable.",
    identity_already_bound:
      "This HireShelby account is connected to another HireShelby identity.",
    pubkey_already_bound:
      "This HireShelby identity is connected to another HireShelby account.",
    not_owner: "Only the community owner can do that.",
    transferee_not_registered:
      "That person needs a connected HireShelby identity before you can transfer ownership to them.",
  };
  const message = messages[error?.code ?? ""] ?? error?.message ?? fallback;
  return correlationId
    ? `${message} Correlation ID: ${correlationId}`
    : message;
}

export function hostedCommunityRelayUrl(community: HostedCommunity) {
  const host = community.normalized_host?.trim();
  return host ? `wss://${host.replace(/^wss?:\/\//, "")}` : null;
}

export function getAccountsAuth() {
  return invoke<AccountsAuth | null>("get_accounts_auth");
}

export function cancelAccountsLogin() {
  return invoke<void>("cancel_accounts_login");
}

export function clearAccountsAuth() {
  return invoke<void>("clear_accounts_auth");
}

export function startAccountsLogin() {
  return invoke<AccountsAuth>("start_accounts_login");
}

export async function loadHostedCommunityAccount(): Promise<HostedCommunityAccount> {
  const [identityResponse, communitiesResponse] = await Promise.all([
    invoke<HostedIdentityResponse>("get_accounts_nostr_identity"),
    invoke<HostedCommunitiesResponse>("list_accounts_communities"),
  ]);
  if (
    identityResponse.error &&
    identityResponse.error.code !== "unauthorized" &&
    !identityResponse.error.setup_needed
  ) {
    throw new Error(
      hostedCommunityErrorMessage(
        identityResponse.error,
        identityResponse.correlation_id,
        "Could not load the connected HireShelby identity.",
      ),
    );
  }
  if (communitiesResponse.error && !communitiesResponse.error.setup_needed) {
    throw new Error(
      hostedCommunityErrorMessage(
        communitiesResponse.error,
        communitiesResponse.correlation_id,
        "Could not load communities.",
      ),
    );
  }
  return {
    identity: identityResponse.identity ?? null,
    communities: communitiesResponse.communities ?? [],
  };
}

export function bindAccountsIdentity() {
  return invoke<HostedIdentityResponse>("bind_accounts_nostr_identity");
}

export function deleteAccountsIdentity() {
  return invoke<HostedIdentityResponse>("delete_accounts_nostr_identity");
}

export function checkHostedCommunityName(name: string) {
  return invoke<HostedCommunityAvailabilityResponse>(
    "check_accounts_community_name",
    { name },
  );
}

export function createHostedCommunity(name: string) {
  return invoke<HostedCommunityMutationResponse>("create_accounts_community", {
    name,
  });
}
