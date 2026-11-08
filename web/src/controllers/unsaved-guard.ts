import type { ReactiveController, ReactiveControllerHost } from 'lit';

export class UnsavedGuard implements ReactiveController {
  private connected = false;
  private guarding = false;
  private readonly warn = (event: BeforeUnloadEvent): void => event.preventDefault();

  constructor(
    host: ReactiveControllerHost,
    private readonly dirty: () => boolean,
  ) {
    host.addController(this);
  }

  hostConnected(): void {
    this.connected = true;
    this.sync();
  }

  hostDisconnected(): void {
    this.connected = false;
    this.sync();
  }

  sync(): void {
    const wanted = this.connected && this.dirty();
    if (wanted === this.guarding) return;
    this.guarding = wanted;
    if (wanted) window.addEventListener('beforeunload', this.warn);
    else window.removeEventListener('beforeunload', this.warn);
  }
}
