import { clearToasts, dismissToast, pushToast, type ToastOptions } from '@/stores/toasts';

export type { ToastOptions } from '@/stores/toasts';

/**
 * Shows a notification in the corner of the screen. The app shell renders the one
 * `<apm-toast-host>` that displays them, so never add another.
 *
 *   toast.success('Moved 12 files');
 *   toast.error(errorMessage(error));
 *   toast.error('Could not read the folder', { timeout: 0 }); // stays until dismissed
 *
 * `message` is plain text (it is never parsed as HTML). Each call returns the toast id for
 * `toast.dismiss(id)`. Toasts sit below an open modal, so report a failure that happens while a
 * modal is open inside the modal (an `alert`) instead.
 */
export const toast = {
  success: (message: string, options?: ToastOptions) => pushToast('success', message, options),
  info: (message: string, options?: ToastOptions) => pushToast('info', message, options),
  warning: (message: string, options?: ToastOptions) => pushToast('warning', message, options),
  error: (message: string, options?: ToastOptions) => pushToast('error', message, options),
  dismiss: dismissToast,
  clear: clearToasts,
};
