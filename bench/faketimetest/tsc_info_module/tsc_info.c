#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/clocksource.h>

/* External variables we want to access */
extern unsigned int tsc_khz;
extern unsigned int cpu_khz;
extern unsigned int lapic_timer_frequency;
// extern struct clocksource clocksource_tsc;

static int __init tsc_info_init(void)
{
    // struct cyc2ns_data data;
    int cpu;

    pr_info("TSC frequency: %u kHz\n", tsc_khz);
    pr_info("CPU frequency: %u kHz\n", cpu_khz);
    
    /* Print clocksource info */
    // pr_info("TSC Clocksource Info:\n");
    // pr_info("  Rating: %d\n", clocksource_tsc.rating);
    // pr_info("  Mask: 0x%llx\n", clocksource_tsc.mask);
    // pr_info("  Mult: %u\n", clocksource_tsc.mult);
    // pr_info("  Shift: %u\n", clocksource_tsc.shift);
    
    // /* Print max_cycles_t for each CPU */
    // for_each_possible_cpu(cpu) {
    //     pr_info("CPU%d cyc2ns info:\n", cpu);
    //     data = *per_cpu_ptr(&cyc2ns, cpu);
    //     pr_info("  Mult: %u\n", data.cyc2ns_mul);
    //     pr_info("  Shift: %u\n", data.cyc2ns_shift);
    //     pr_info("  Offset: %lld\n", data.cyc2ns_offset);
    // }

    return 0;
}

static void __exit tsc_info_exit(void)
{
    pr_info("TSC info module unloaded\n");
}

module_init(tsc_info_init);
module_exit(tsc_info_exit);

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("Display TSC calibration info");