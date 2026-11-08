import type { SelectionDto } from '@/api/generated/SelectionDto';
import { setFilter } from '@/stores/filters';
import { linkTo, locations, navigate, type PaneId, setLocation, type ToolId } from '@/stores/nav';
import { setHandoff, setPinnedBase } from '@/stores/tool-state';
import type { ToolConfig } from '@/tools/framework/tool-config';
import { EMPTY_FILTER } from '@/util/filter';
import { formatCount } from '@/util/format-count';

export interface SelectionHandoff {
  /** Pass the target config when its source base is pinned; a tool id is enough otherwise. */
  target: ToolId | Pick<ToolConfig, 'id' | 'panes'>;
  origin: PaneId;
  selection: SelectionDto;
  label?: string;
}

export function handOffSelection({ target, origin, selection, label }: SelectionHandoff): void {
  const tool = typeof target === 'string' ? target : target.id;
  const location = locations[origin].peek();
  setFilter(tool, { ...EMPTY_FILTER, selection_id: selection.id });
  setHandoff(tool, {
    label: label ?? (selection.label || `${formatCount(selection.item_count)} files`),
  });
  setLocation('source', location);
  if (typeof target === 'string' ? target === 'move' : target.panes.source.baseMode === 'pinned') {
    setPinnedBase(tool, 'source', location);
  }
  navigate(linkTo(`/${tool}`));
}
