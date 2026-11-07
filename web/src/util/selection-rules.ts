/** How the user reached an item: the grid reports it in the `card-activate` event. */
export type ActivationVia = 'checkbox' | 'card' | 'space' | 'enter';

/** What the grid reports. `toggle` is Cmd (macOS) or Ctrl held during the activation. */
export interface Activation {
  id: number;
  via: ActivationVia;
  shift: boolean;
  toggle: boolean;
}

export interface SelectionState {
  /** True while at least one item of the pane is selected. */
  active: boolean;
}

export type Decision =
  | { type: 'toggle'; id: number }
  | { type: 'range'; id: number }
  | { type: 'open'; id: number };

/**
 * Turns an activation into the one thing the selection (or the viewer) must do. Pure: the
 * `SelectionController` applies the result.
 *
 * | via        | shift | toggle | active | decision                                  |
 * | ---------- | ----- | ------ | ------ | ----------------------------------------- |
 * | `enter`    | any   | any    | any    | open                                      |
 * | `space`    | yes   | any    | any    | range                                     |
 * | `space`    | no    | any    | any    | toggle                                    |
 * | `checkbox` | yes   | any    | any    | range                                     |
 * | `checkbox` | no    | any    | any    | toggle                                    |
 * | `card`     | yes   | any    | any    | range (with no anchor: just that item)    |
 * | `card`     | no    | yes    | any    | toggle                                    |
 * | `card`     | no    | no     | yes    | toggle (clicking selects while active)    |
 * | `card`     | no    | no     | no     | open (clicking opens while not active)    |
 *
 * Shift wins when Cmd/Ctrl is held too.
 */
export function interpret(activation: Activation, state: SelectionState): Decision {
  const { id, via, shift, toggle } = activation;
  if (via === 'enter') return { type: 'open', id };
  if (shift) return { type: 'range', id };
  if (via !== 'card' || toggle || state.active) return { type: 'toggle', id };
  return { type: 'open', id };
}
