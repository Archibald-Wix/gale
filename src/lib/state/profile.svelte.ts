import { invoke } from '$lib/invoke';
import type { CommunityInfo, ProfileInfo } from '$lib/models';
import { listen } from '@tauri-apps/api/event';

class Communities {
	all: CommunityInfo[] = $state([]);
	active: CommunityInfo | undefined = $state();

	setActive(id: number) {
		this.active = getCommunity(id);
		setLocalStorageInt('activeCommunity', communities.active?.id ?? 1);
	}
}

const communities = new Communities();

class Profiles {
	active: ProfileInfo | undefined = $state();

	get activeId() {
		if (this.active !== undefined) {
			return this.active.id;
		}

		console.warn('no active profile');
		return 1;
	}

	async setActive(id: number) {
		this.active = await invoke('profile', 'get', { id });
		setLocalStorageInt('activeProfile', profiles.active?.id ?? 1);
	}
}

const profiles = new Profiles();

listen<ProfileInfo>('profile-update', async ({ payload }) => {
	if (profiles.active?.id === payload.id) {
		profiles.active = payload;
	}
})

fetchCommunities();

let activeProfileId = getLocalStorageInt('activeProfile', 1);
profiles.setActive(activeProfileId);

function getCommunity(id: number): CommunityInfo {
	return communities.all.find((community) => community.id === id)!;
}

function getLocalStorageInt(key: string, def: number): number {
	let value = localStorage.getItem(key);
	if (value === null) {
		return def;
	}
	return parseInt(value);
}

function setLocalStorageInt(key: string, value: number) {
	localStorage.setItem(key, value.toString());
}

async function fetchCommunities() {
	let id = getLocalStorageInt('activeCommunity', 1);
	communities.all = await invoke('core', 'get_communities');
	communities.active = getCommunity(id);
}

export { communities, profiles };
