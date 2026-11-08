import { vi } from 'vitest';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { FolderEntry } from '@/api/generated/FolderEntry';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import type { ItemsRequest } from '@/api/generated/ItemsRequest';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { fileItem } from '@/test/file-item';

/** An answer that is an error: the server sends `{ error: { code, message } }` with a status. */
export interface ServerFailure {
  status: number;
  code: string;
  message: string;
}

/** The endpoints a pane talks to; each can be held back with `hold`. */
export type Endpoint = 'volumes' | 'folders' | 'items' | 'ids';

export interface BrowseServerState {
  /** Answer of `GET /api/volumes`. */
  volumes: readonly VolumeDto[];
  /** When set, `GET /api/volumes` fails with this message instead. */
  volumesFailure: string | null;
  /** Answer of `POST /api/browse/folders` for a request: change it between requests. */
  folders: (request: BrowseRequest) => FoldersResponse | ServerFailure;
  /** Files in every listing; `POST /api/browse/items` pages through them. */
  total: number;
  item: (index: number) => FileItem;
  /** Answer of `POST /api/browse/ids`. */
  ids: (request: ItemsRequest) => readonly number[];
}

export interface BrowseServerCalls {
  volumes: number;
  folders: BrowseRequest[];
  items: ItemsRequest[];
  ids: ItemsRequest[];
  /** The abort signal of each `ids` call: a selection that is dropped aborts its request. */
  idSignals: (AbortSignal | null | undefined)[];
}

export interface BrowseServer {
  state: BrowseServerState;
  calls: BrowseServerCalls;
  /** Keeps the answers of an endpoint back until `release`. */
  hold(endpoint: Endpoint): void;
  release(endpoint: Endpoint): void;
}

export const VOLUME_PHOTOS: VolumeDto = { id: 1, name: 'photos', writable: true, online: true };
export const VOLUME_BACKUP: VolumeDto = { id: 2, name: 'backup', writable: false, online: true };

export function footerCounts(counts: Partial<FooterCounts> = {}): FooterCounts {
  return { in_folder: 0, in_subfolders: 0, ...counts };
}

/** What `POST /api/browse/folders` answers for these subfolders. */
export function foldersAnswer(
  folders: readonly FolderEntry[],
  counts: Partial<FooterCounts> = {},
): FoldersResponse {
  return { folders: [...folders], counts: footerCounts(counts) };
}

function isFailure(answer: FoldersResponse | ServerFailure): answer is ServerFailure {
  return 'code' in answer;
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

function failureResponse({ status, code, message }: ServerFailure): Response {
  return json({ error: { code, message } }, status);
}

function parse<T>(init: RequestInit): T {
  return JSON.parse(String(init.body)) as T;
}

/**
 * Stubs the global `fetch` with a server for what a pane asks: `GET /api/volumes` and
 * `POST /api/browse/{folders,items,ids}`. It answers from `state`, which a test may change
 * between requests, and records every request in `calls`. By default there are the volumes
 * `photos` (id 1) and `backup` (id 2), no subfolders and no files. Undo it with
 * `vi.unstubAllGlobals()`.
 */
export function stubBrowseServer(initial: Partial<BrowseServerState> = {}): BrowseServer {
  const state: BrowseServerState = {
    volumes: [VOLUME_PHOTOS, VOLUME_BACKUP],
    volumesFailure: null,
    folders: () => foldersAnswer([]),
    total: 0,
    item: (index) => fileItem({ id: index + 1, name: `IMG_${index}.jpg` }),
    ids: () => [],
    ...initial,
  };
  const calls: BrowseServerCalls = { volumes: 0, folders: [], items: [], ids: [], idSignals: [] };
  const gates = new Map<Endpoint, { open: () => void; closed: Promise<void> }>();

  async function answer(url: string, init: RequestInit): Promise<Response> {
    switch (url) {
      case '/api/volumes': {
        calls.volumes += 1;
        await gates.get('volumes')?.closed;
        if (state.volumesFailure !== null) {
          return failureResponse({ status: 500, code: 'internal', message: state.volumesFailure });
        }
        return json(state.volumes);
      }
      case '/api/browse/folders': {
        const body = parse<BrowseRequest>(init);
        calls.folders.push(body);
        await gates.get('folders')?.closed;
        const result = state.folders(body);
        return isFailure(result) ? failureResponse(result) : json(result);
      }
      case '/api/browse/items': {
        const body = parse<ItemsRequest>(init);
        calls.items.push(body);
        await gates.get('items')?.closed;
        const items: FileItem[] = [];
        const end = Math.min(state.total, body.offset + body.limit);
        for (let index = body.offset; index < end; index += 1) items.push(state.item(index));
        return json({ items, total: state.total, offset: body.offset });
      }
      case '/api/browse/ids': {
        const body = parse<ItemsRequest>(init);
        calls.ids.push(body);
        calls.idSignals.push(init.signal);
        await gates.get('ids')?.closed;
        return json({ ids: [...state.ids(body)] });
      }
      default:
        return failureResponse({ status: 404, code: 'not_found', message: `No stub for ${url}` });
    }
  }

  vi.stubGlobal(
    'fetch',
    vi.fn((url: string, init: RequestInit) => answer(url, init)),
  );

  return {
    state,
    calls,
    hold(endpoint) {
      if (gates.has(endpoint)) return;
      let open = () => {};
      const closed = new Promise<void>((resolve) => {
        open = resolve;
      });
      gates.set(endpoint, { open, closed });
    },
    release(endpoint) {
      gates.get(endpoint)?.open();
      gates.delete(endpoint);
    },
  };
}
