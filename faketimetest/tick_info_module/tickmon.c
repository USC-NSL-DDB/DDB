#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/proc_fs.h>
#include <linux/seq_file.h>
#include <linux/jiffies.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("TICK_NSEC Display Module");

static struct proc_dir_entry *proc_entry;

static int tick_nsec_show(struct seq_file *m, void *v)
{
    seq_printf(m, "TICK_NSEC: %lu nanoseconds\n", TICK_NSEC);
    seq_printf(m, "HZ: %lu\n", HZ);
    return 0;
}

static int tick_nsec_open(struct inode *inode, struct file *file)
{
    return single_open(file, tick_nsec_show, NULL);
}

static const struct proc_ops tick_nsec_fops = {
    .proc_open = tick_nsec_open,
    .proc_read = seq_read,
    .proc_lseek = seq_lseek,
    .proc_release = single_release,
};

static int __init tick_nsec_init(void)
{
    printk(KERN_INFO "TICK_NSEC: %lu nanoseconds\n", TICK_NSEC);
    
    proc_entry = proc_create("tick_nsec", 0444, NULL, &tick_nsec_fops);
    if (!proc_entry)
        return -ENOMEM;
    
    return 0;
}

static void __exit tick_nsec_exit(void)
{
    proc_remove(proc_entry);
}

module_init(tick_nsec_init);
module_exit(tick_nsec_exit);