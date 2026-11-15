import { toast } from '@/components/toast';

export async function registerPwa(): Promise<void> {
  if (!import.meta.env.PROD || !window.isSecureContext || !('serviceWorker' in navigator)) return;
  try {
    await navigator.serviceWorker.register('/sw.js', { scope: '/', updateViaCache: 'none' });
  } catch {
    toast.error('Offline support could not be installed. Reload to try again.');
  }
}
